//! Two-pass capture: pass 1 walks the tree, assigning pre-order ids while
//! building OpNodes; pass 2 resolves the raw pointers of op_next / op_other /
//! LOOP branch targets into node ids. The execution-order chain is then
//! closed within the IR, so the subsequent CFG construction can be done in
//! pure Rust (inspect-core).

use std::collections::HashMap;

use inspect_core::ir::{DerefStep, OpClass, OpDetail, OpNode, PadEntry, SubIr, SvLit};
use libperl_rs::{Cop, Cv, Gv, Op, Perl};
use libperl_sys::{
    OPclass, OPf_KIDS, Perl_op_class, PerlInterpreter, methop, op, padop, sv, svop, svtype,
};

use crate::raw::*;

/// List of op names in execution order (CvSTART → op_next). The substance of M0's op_names.
pub fn exec_op_names(cv: Cv) -> Result<Vec<String>, String> {
    let Some(start) = cv.start_op() else {
        return Err("cannot analyze an XSUB (no op tree)".into());
    };
    Ok(start
        .next_iter()
        .map(|o| op_name_of_type(o.op_type_raw() as u16))
        .collect())
}

/// Copy the coderef's entire OP tree into a SubIr
pub fn capture_sub(perl: &Perl, cv: Cv) -> Result<SubIr, String> {
    if cv.is_xsub() {
        return Err("cannot analyze an XSUB (no op tree)".into());
    }
    let root = cv.root();
    if root.is_null() {
        return Err("subroutine has no op tree".into());
    }
    let file = cv.file();
    let proto = cv.proto();

    let mut cap = Capturer {
        my_perl: perl.as_ptr(),
        cv,
        ids: HashMap::new(),
        raws: Vec::new(),
        min_line: u32::MAX,
        max_line: 0,
    };
    let mut root_node = cap.walk(root);
    cap.resolve(&mut root_node);

    Ok(SubIr {
        file,
        lines: if cap.max_line > 0 && cap.min_line <= cap.max_line {
            Some((cap.min_line, cap.max_line))
        } else {
            None
        },
        proto,
        pad: capture_pad(cv),
        start_id: cap.lookup(cv.start()),
        root: root_node,
    })
}

/// Raw pointers kept for pass 2 (pushed onto raws in the same order as ids)
struct RawLinks {
    next: *const op,
    other: *const op,
    redo: *const op,
    loop_next: *const op,
    loop_last: *const op,
}

struct Capturer {
    my_perl: *mut PerlInterpreter,
    cv: Cv,
    ids: HashMap<usize, u32>,
    raws: Vec<RawLinks>,
    min_line: u32,
    max_line: u32,
}

impl Capturer {
    /// pass 1: build OpNodes in pre-order
    fn walk(&mut self, o: *const op) -> OpNode {
        let id = self.raws.len() as u32;
        self.ids.insert(o as usize, id);

        let ty = unsafe { (*o).op_type() };
        let flags = unsafe { (*o).op_flags };
        let private = unsafe { (*o).op_private };
        let targ = unsafe { (*o).op_targ };
        let cls = unsafe { Perl_op_class(self.my_perl, o) };

        let mut raw = RawLinks {
            next: unsafe { (*o).op_next as *const op },
            other: std::ptr::null(),
            redo: std::ptr::null(),
            loop_next: std::ptr::null(),
            loop_last: std::ptr::null(),
        };
        match cls {
            OPclass::OPclass_LOGOP => {
                raw.other = unsafe { (*(o as *const libperl_sys::logop)).op_other };
            }
            OPclass::OPclass_LOOP => {
                let lp = o as *const libperl_sys::loop_;
                raw.redo = unsafe { (*lp).op_redoop };
                raw.loop_next = unsafe { (*lp).op_nextop };
                raw.loop_last = unsafe { (*lp).op_lastop };
            }
            _ => {}
        }
        self.raws.push(raw);

        // OP_NULL (op_type == 0) keeps its pre-nulling type in op_targ
        let was = if ty == 0 && targ > 0 {
            Some(op_name_of_type(targ as u16))
        } else {
            None
        };

        let detail = self.extract_detail(o, ty, cls, flags);

        let mut kids = Vec::new();
        if (flags as u32 & OPf_KIDS) != 0 {
            let this = unsafe { Op::from_raw_unchecked(o) };
            for kid in this.kids() {
                kids.push(self.walk(kid.as_ptr()));
            }
        }

        OpNode {
            id,
            name: op_name_of_type(ty),
            op_type: ty,
            class: opclass_to_ir(cls),
            was,
            flags,
            private,
            targ: targ as u64,
            next: None,
            other: None,
            detail,
            kids,
        }
    }

    fn extract_detail(&mut self, o: *const op, ty: u16, cls: OPclass, flags: u8) -> OpDetail {
        match cls {
            OPclass::OPclass_COP => {
                let c = unsafe { Cop::from_raw_unchecked(o as *const libperl_sys::COP) };
                let line = c.line();
                let file = c.file();
                if line > 0 {
                    self.min_line = self.min_line.min(line);
                    self.max_line = self.max_line.max(line);
                }
                OpDetail::Cop { file, line }
            }
            OPclass::OPclass_SVOP => {
                let s = o as *const svop;
                let sv = {
                    let p = unsafe { (*s).op_sv };
                    if p.is_null() {
                        // ithreads: const SVs are placed in the pad
                        PAD_BASE_SV(self.cv.padlist(), unsafe { (*o).op_targ })
                    } else {
                        p as *const sv
                    }
                };
                sv_detail(sv)
            }
            OPclass::OPclass_PADOP => {
                // ithreads: GV references become PADOPs and are resolved via the pad
                let p = o as *const padop;
                let sv = PAD_BASE_SV(self.cv.padlist(), unsafe { (*p).op_padix });
                sv_detail(sv)
            }
            OPclass::OPclass_METHOP => {
                if (flags as u32 & OPf_KIDS) != 0 {
                    OpDetail::Method { name: None } // dynamic method
                } else {
                    let m = o as *const methop;
                    let sv = {
                        let p = unsafe { (*m).op_u.op_meth_sv };
                        if p.is_null() {
                            PAD_BASE_SV(self.cv.padlist(), unsafe { (*o).op_targ })
                        } else {
                            p as *const sv
                        }
                    };
                    let name = match sv_lit(sv) {
                        SvLit::Pv(s) => Some(s),
                        _ => None,
                    };
                    OpDetail::Method { name }
                }
            }
            OPclass::OPclass_LOOP => OpDetail::Loop {
                redo: None,
                next: None,
                last: None,
            }, // resolved in pass 2
            OPclass::OPclass_PMOP => OpDetail::Pm { pattern: None },
            OPclass::OPclass_UNOP_AUX => {
                let aux = unsafe { (*(o as *const libperl_sys::unop_aux)).op_aux };
                match op_name_of_type(ty).as_str() {
                    "argcheck" => {
                        let a = aux as *const libperl_sys::op_argcheck_aux;
                        if a.is_null() {
                            OpDetail::Aux("argcheck".into())
                        } else {
                            let slurpy = unsafe { (*a).slurpy } as u8;
                            OpDetail::ArgCheck {
                                params: unsafe { (*a).params },
                                opt: unsafe { (*a).opt_params },
                                slurpy: if slurpy == 0 {
                                    None
                                } else {
                                    Some(slurpy as char)
                                },
                            }
                        }
                    }
                    // pp_argelem uses the op_aux pointer value itself as the index
                    "argelem" => OpDetail::ArgElem { index: aux as u64 },
                    "multideref" => self.decode_multideref(aux),
                    "multiconcat" => self.decode_multiconcat(aux),
                    // Other UNOP_AUX ops are not decoded (keep the op name as a marker)
                    other => OpDetail::Aux(other.to_string()),
                }
            }
            _ => OpDetail::None,
        }
    }

    /// Decode the aux array of multideref.
    ///
    /// The format is that of op.h (stable since 5.22): the first item is the
    /// actions word, one action per 7 bits. In each action the low 4 bits are
    /// the base kind, 0x30 the index kind, and 0x40 the last-element flag.
    /// Subsequent items (pad_offset / sv / iv) are consumed according to the
    /// base/index kinds.
    fn decode_multideref(&self, aux: *mut libperl_sys::UNOP_AUX_item) -> OpDetail {
        // Widen the libperl-sys-generated MDEREF_* (u32) to the actions width (UV=u64)
        const MDEREF_ACTION_MASK: u64 = libperl_sys::MDEREF_ACTION_MASK as u64;
        const MDEREF_INDEX_MASK: u64 = libperl_sys::MDEREF_INDEX_MASK as u64;
        const MDEREF_INDEX_CONST: u64 = libperl_sys::MDEREF_INDEX_const as u64;
        const MDEREF_INDEX_PADSV: u64 = libperl_sys::MDEREF_INDEX_padsv as u64;
        const MDEREF_INDEX_GVSV: u64 = libperl_sys::MDEREF_INDEX_gvsv as u64;
        const MDEREF_FLAG_LAST: u64 = libperl_sys::MDEREF_FLAG_last as u64;
        const MDEREF_SHIFT: u32 = libperl_sys::MDEREF_SHIFT;

        if aux.is_null() {
            return OpDetail::Aux("multideref".into());
        }
        let mut steps: Vec<DerefStep> = Vec::new();
        unsafe {
            let mut items = aux;
            let mut actions = (*items).uv;
            // Runaway guard (real chains are at most a few steps)
            while steps.len() < 64 {
                let kind = actions & MDEREF_ACTION_MASK;
                if kind == 0 {
                    // MDEREF_reload: the next item is a new actions word
                    items = items.add(1);
                    actions = (*items).uv;
                    if actions == 0 {
                        break;
                    }
                    continue;
                }
                let (container, base) = match kind {
                    1 => ("array", "stack"),
                    2 => ("array", "gvsv"),
                    3 => ("array", "padsv"),
                    4 => ("array", "chain"),
                    5 => ("array", "padav"),
                    6 => ("array", "gvav"),
                    8 => ("hash", "stack"),
                    9 => ("hash", "gvsv"),
                    10 => ("hash", "padsv"),
                    11 => ("hash", "chain"),
                    12 => ("hash", "padhv"),
                    13 => ("hash", "gvhv"),
                    // Unknown action: the boundaries of subsequent items are
                    // unknown, so stop here and return what has been decoded
                    _ => break,
                };
                let (base_targ, base_name) = match base {
                    "padsv" | "padav" | "padhv" => {
                        items = items.add(1);
                        (Some((*items).pad_offset as u64), None)
                    }
                    "gvsv" | "gvav" | "gvhv" => {
                        items = items.add(1);
                        let name = match sv_lit(self.aux_item_sv(items)) {
                            SvLit::Glob { name, stash } => match stash.as_deref() {
                                Some("main") | None => Some(name),
                                Some(pkg) => Some(format!("{}::{}", pkg, name)),
                            },
                            _ => None,
                        };
                        (None, name)
                    }
                    _ => (None, None),
                };
                let key = match actions & MDEREF_INDEX_MASK {
                    MDEREF_INDEX_CONST => {
                        items = items.add(1);
                        if container == "array" {
                            Some((*items).iv.to_string())
                        } else {
                            match sv_lit(self.aux_item_sv(items)) {
                                SvLit::Pv(s) => Some(s),
                                SvLit::Iv(i) => Some(i.to_string()),
                                SvLit::Uv(u) => Some(u.to_string()),
                                _ => None,
                            }
                        }
                    }
                    MDEREF_INDEX_PADSV => {
                        items = items.add(1);
                        let po = (*items).pad_offset as u64;
                        Some(
                            self.pad_name_at(po)
                                .unwrap_or_else(|| format!("$pad{}", po)),
                        )
                    }
                    MDEREF_INDEX_GVSV => {
                        items = items.add(1);
                        match sv_lit(self.aux_item_sv(items)) {
                            SvLit::Glob { name, .. } => Some(format!("${}", name)),
                            _ => None,
                        }
                    }
                    _ => None, // INDEX_none: the index is computed by a preceding op
                };
                steps.push(DerefStep {
                    container: container.into(),
                    base: base.into(),
                    base_targ,
                    base_name,
                    key,
                });
                if actions & MDEREF_FLAG_LAST != 0 {
                    break;
                }
                actions >>= MDEREF_SHIFT;
            }
        }
        OpDetail::MultiDeref { steps }
    }

    /// Decode the aux array of multiconcat.
    ///
    /// Layout (perl.h "multiconcat" section, S_maybe_multiconcat in op.c,
    /// pp_multiconcat in pp_hot.c): aux[IX_NARGS] is the operand count,
    /// aux[IX_PLAIN_PV/LEN] the concatenated constant string in perl's native
    /// (latin1) encoding or NULL when it is utf8-only, aux[IX_UTF8_PV/LEN]
    /// the utf8 encoding (always set; it aliases the plain buffer when the
    /// string is invariant), and from aux[IX_LENGTHS] follow nargs+1
    /// per-segment lengths (-1 = no constant at that position). When the
    /// plain and utf8 buffers differ, a second length set for the utf8
    /// buffer follows the first. The utf8 buffer is decoded here so that the
    /// pieces are the exact character sequence perl sees regardless of the
    /// source encoding.
    fn decode_multiconcat(&self, aux: *mut libperl_sys::UNOP_AUX_item) -> OpDetail {
        const IX_NARGS: usize = libperl_sys::PERL_MULTICONCAT_IX_NARGS as usize;
        const IX_PLAIN_PV: usize = libperl_sys::PERL_MULTICONCAT_IX_PLAIN_PV as usize;
        const IX_PLAIN_LEN: usize = libperl_sys::PERL_MULTICONCAT_IX_PLAIN_LEN as usize;
        const IX_UTF8_PV: usize = libperl_sys::PERL_MULTICONCAT_IX_UTF8_PV as usize;
        const IX_UTF8_LEN: usize = libperl_sys::PERL_MULTICONCAT_IX_UTF8_LEN as usize;
        const IX_LENGTHS: usize = libperl_sys::PERL_MULTICONCAT_IX_LENGTHS as usize;
        const MAXARG: isize = libperl_sys::PERL_MULTICONCAT_MAXARG as isize;

        let undecoded = || OpDetail::Aux("multiconcat".into());
        if aux.is_null() {
            return undecoded();
        }
        unsafe {
            let nargs = (*aux.add(IX_NARGS)).ssize;
            if !(0..=MAXARG).contains(&nargs) {
                return undecoded();
            }
            let plain = (*aux.add(IX_PLAIN_PV)).pv;
            let utf8 = (*aux.add(IX_UTF8_PV)).pv;
            // Defensive: fall back to the plain buffer if utf8 is unset
            let (pv, len, lens_ix) = if utf8.is_null() {
                (plain, (*aux.add(IX_PLAIN_LEN)).ssize, IX_LENGTHS)
            } else if plain.is_null() || plain == utf8 {
                (utf8, (*aux.add(IX_UTF8_LEN)).ssize, IX_LENGTHS)
            } else {
                // Distinct plain/utf8 buffers: the utf8 lengths are the second set
                (
                    utf8,
                    (*aux.add(IX_UTF8_LEN)).ssize,
                    IX_LENGTHS + nargs as usize + 1,
                )
            };
            if pv.is_null() || len < 0 {
                return undecoded();
            }
            let bytes = std::slice::from_raw_parts(pv as *const u8, len as usize);
            let mut pieces: Vec<Option<String>> = Vec::with_capacity(nargs as usize + 1);
            let mut off = 0usize;
            for i in 0..=(nargs as usize) {
                let seg = (*aux.add(lens_ix + i)).ssize;
                if seg < 0 {
                    pieces.push(None);
                    continue;
                }
                let end = off + seg as usize;
                if end > bytes.len() {
                    // Inconsistent lengths: do not read past the constant
                    return undecoded();
                }
                pieces.push(Some(String::from_utf8_lossy(&bytes[off..end]).into_owned()));
                off = end;
            }
            OpDetail::MultiConcat { pieces }
        }
    }

    /// Extract the SV from a UNOP_AUX item. Under ithreads, SV items are
    /// stored as pad offsets (the UNOP_AUX_item_sv macro in perl.h:
    /// `PAD_SVl((item)->pad_offset)`). This project assumes a threaded perl
    /// (on a non-threaded perl this would read (*item).sv directly)
    unsafe fn aux_item_sv(&self, item: *const libperl_sys::UNOP_AUX_item) -> *const sv {
        PAD_BASE_SV(self.cv.padlist(), unsafe { (*item).pad_offset })
    }

    /// Look up a variable name by pad index (for displaying multideref padsv indices)
    fn pad_name_at(&self, po: u64) -> Option<String> {
        let pn = self.cv.pad_names().nth(po as usize)??;
        pn.pv().filter(|s| !s.is_empty())
    }

    /// pass 2: resolve raw pointers into node ids
    fn resolve(&self, node: &mut OpNode) {
        let raw = &self.raws[node.id as usize];
        node.next = self.lookup(raw.next);
        node.other = self.lookup(raw.other);
        if matches!(node.detail, OpDetail::Loop { .. }) {
            node.detail = OpDetail::Loop {
                redo: self.lookup(raw.redo),
                next: self.lookup(raw.loop_next),
                last: self.lookup(raw.loop_last),
            };
        }
        for k in &mut node.kids {
            self.resolve(k);
        }
    }

    fn lookup(&self, p: *const op) -> Option<u32> {
        if p.is_null() {
            None
        } else {
            self.ids.get(&(p as usize)).copied()
        }
    }
}

fn opclass_to_ir(cls: OPclass) -> OpClass {
    match cls {
        OPclass::OPclass_NULL => OpClass::Null,
        OPclass::OPclass_BASEOP => OpClass::Base,
        OPclass::OPclass_UNOP => OpClass::Unop,
        OPclass::OPclass_BINOP => OpClass::Binop,
        OPclass::OPclass_LOGOP => OpClass::Logop,
        OPclass::OPclass_LISTOP => OpClass::Listop,
        OPclass::OPclass_PMOP => OpClass::Pmop,
        OPclass::OPclass_SVOP => OpClass::Svop,
        OPclass::OPclass_PADOP => OpClass::Padop,
        OPclass::OPclass_PVOP => OpClass::Pvop,
        OPclass::OPclass_LOOP => OpClass::Loop,
        OPclass::OPclass_COP => OpClass::Cop,
        OPclass::OPclass_METHOP => OpClass::Methop,
        OPclass::OPclass_UNOP_AUX => OpClass::UnopAux,
    }
}

fn sv_detail(sv: *const sv) -> OpDetail {
    match sv_lit(sv) {
        SvLit::Glob { name, stash } => OpDetail::Gv { name, stash },
        lit => OpDetail::Const(lit),
    }
}

/// Copy an SV out as a literal value (built from the official API only)
fn sv_lit(sv: *const sv) -> SvLit {
    if sv.is_null() {
        return SvLit::Other("NULL".into());
    }
    // Globs with a GP (libperl-rs's Gv checks via isGV_with_GP) take top
    // priority. Mutually exclusive with PVCV/REGEXP, so the check order does
    // not matter semantically.
    if let Some(gv) = Gv::from_sv(sv as *mut _) {
        return SvLit::Glob {
            name: gv.name().unwrap_or_default(),
            stash: gv.stash_name(),
        };
    }
    let t = SvTYPE(sv);
    match t {
        svtype::SVt_PVCV => return SvLit::Code,
        svtype::SVt_REGEXP => return SvLit::Other("REGEXP".into()),
        _ => {}
    }
    if (t as u32) >= svtype::SVt_PVAV as u32 {
        return SvLit::Other(format!("svtype:{:?}", t));
    }
    if SvROK(sv) {
        return SvLit::RefTo(Box::new(sv_lit(SvRV(sv))));
    }
    unsafe {
        if libperl_sys::SvIOK(sv) != 0 {
            if libperl_sys::SvIsUV(sv) != 0 {
                SvLit::Uv(libperl_sys::SvUVX(sv) as u64)
            } else {
                SvLit::Iv(libperl_sys::SvIVX(sv) as i64)
            }
        } else if libperl_sys::SvNOK(sv) != 0 {
            SvLit::Nv(libperl_sys::SvNVX(sv))
        } else if libperl_sys::SvPOK(sv) != 0 {
            let pv = libperl_sys::SvPVX_const(sv);
            if pv.is_null() {
                SvLit::Undef
            } else {
                // May contain NUL, so read SvCUR bytes
                let len = libperl_sys::SvCUR(sv);
                let bytes = std::slice::from_raw_parts(pv as *const u8, len as usize);
                SvLit::Pv(String::from_utf8_lossy(bytes).into_owned())
            }
        } else {
            SvLit::Undef
        }
    }
}

/// Collect named pad entries (table of targ → name / declared type)
fn capture_pad(cv: Cv) -> Vec<PadEntry> {
    let mut out = Vec::new();
    for (ix, slot) in cv.pad_names().enumerate() {
        let Some(pn) = slot else { continue };
        let name = pn.pv();
        if name.as_deref().is_none_or(str::is_empty) {
            continue; // unnamed slots (temporaries / consts) are not listed
        }
        out.push(PadEntry {
            ix: ix as u32,
            name,
            typ: pn.type_stash_name(),
            flags: PadnameFLAGS(pn.as_ptr()),
        });
    }
    out
}
