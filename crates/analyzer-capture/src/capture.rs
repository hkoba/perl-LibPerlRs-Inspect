//! 2 パスキャプチャ: pass 1 で木を歩き pre-order id を採番しつつ
//! OpNode を構築、pass 2 で op_next / op_other / LOOP 分岐先の生ポインタを
//! ノード id へ解決する。実行順チェーンが IR 内に閉じるので、以降の
//! CFG 構築は純 Rust (analyzer-core) でできる。

use std::collections::HashMap;

use analyzer_core::ir::{DerefStep, OpClass, OpDetail, OpNode, PadEntry, SubIr, SvLit};
use libperl_rs::Perl;
use libperl_sys::{
    OPclass, OPf_KIDS, Perl_op_class, PerlInterpreter, SV, cop, methop, op, padop, sv, svop,
    svtype,
};

use crate::raw::*;

/// 実行順 (CvSTART → op_next) の op 名リスト。M0 の op_names の実体。
pub fn exec_op_names(code: *const SV) -> Result<Vec<String>, String> {
    let cv = coderef_to_cv(code)?;
    let start = CvSTART(cv);
    if start.is_null() {
        return Err("cannot analyze an XSUB (no op tree)".into());
    }
    Ok(next_iter(start).map(op_name).collect())
}

/// coderef の OP ツリー全体を SubIr に写し取る
pub fn capture_sub(perl: &Perl, code: *const SV) -> Result<SubIr, String> {
    let cv = coderef_to_cv(code)?;
    if CvISXSUB(cv) {
        return Err("cannot analyze an XSUB (no op tree)".into());
    }
    let root = CvROOT(cv);
    if root.is_null() {
        return Err("subroutine has no op tree".into());
    }

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
        file: CvFILE(cv),
        lines: if cap.max_line > 0 && cap.min_line <= cap.max_line {
            Some((cap.min_line, cap.max_line))
        } else {
            None
        },
        proto: None,
        pad: capture_pad(cv),
        start_id: cap.lookup(CvSTART(cv)),
        root: root_node,
    })
}

/// pass 2 用に温存する生ポインタ (id と同順で raws に積む)
struct RawLinks {
    next: *const op,
    other: *const op,
    redo: *const op,
    loop_next: *const op,
    loop_last: *const op,
}

struct Capturer {
    my_perl: *mut PerlInterpreter,
    cv: *const libperl_sys::cv,
    ids: HashMap<usize, u32>,
    raws: Vec<RawLinks>,
    min_line: u32,
    max_line: u32,
}

impl Capturer {
    /// pass 1: pre-order で OpNode を構築
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

        // OP_NULL (op_type == 0) は null 化前の型を op_targ に持つ
        let was = if ty == 0 && targ > 0 {
            Some(op_name_of_type(targ as u16))
        } else {
            None
        };

        let detail = self.extract_detail(o, ty, cls, flags);

        let mut kids = Vec::new();
        if (flags as u32 & OPf_KIDS) != 0 {
            let mut k = op_first(o);
            while !k.is_null() {
                kids.push(self.walk(k));
                k = op_sibling(k);
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
                let c = o as *const cop;
                let line = CopLINE(c);
                let file = CopFILE(c);
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
                        // ithreads: const SV は pad に置かれる
                        PAD_BASE_SV(CvPADLIST(self.cv), unsafe { (*o).op_targ })
                    } else {
                        p as *const sv
                    }
                };
                sv_detail(sv)
            }
            OPclass::OPclass_PADOP => {
                // ithreads: GV 参照は PADOP になり pad 経由で解決する
                let p = o as *const padop;
                let sv = PAD_BASE_SV(CvPADLIST(self.cv), unsafe { (*p).op_padix });
                sv_detail(sv)
            }
            OPclass::OPclass_METHOP => {
                if (flags as u32 & OPf_KIDS) != 0 {
                    OpDetail::Method { name: None } // 動的メソッド
                } else {
                    let m = o as *const methop;
                    let sv = {
                        let p = unsafe { (*m).op_u.op_meth_sv };
                        if p.is_null() {
                            PAD_BASE_SV(CvPADLIST(self.cv), unsafe { (*o).op_targ })
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
            }, // pass 2 で解決
            OPclass::OPclass_PMOP => OpDetail::Pm { pattern: None },
            OPclass::OPclass_UNOP_AUX => {
                let aux = unsafe { (*(o as *const libperl_sys::unop_aux)).op_aux };
                match op_name_of_type(ty).as_str() {
                    "argcheck" => {
                        // struct op_argcheck_aux (op.h) は bindgen 未生成のため
                        // 5.42 のレイアウトをミラー (libperl-sys の allowlist
                        // 追加候補)
                        #[repr(C)]
                        struct OpArgcheckAux {
                            params: u64,
                            opt_params: u64,
                            slurpy: std::os::raw::c_char,
                        }
                        let a = aux as *const OpArgcheckAux;
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
                    // pp_argelem は op_aux ポインタの値そのものを添字に使う
                    "argelem" => OpDetail::ArgElem { index: aux as u64 },
                    "multideref" => self.decode_multideref(aux),
                    // multiconcat 等は未デコード (op 名を目印に残す)
                    other => OpDetail::Aux(other.to_string()),
                }
            }
            _ => OpDetail::None,
        }
    }

    /// multideref の aux 配列をデコードする。
    ///
    /// 形式は op.h (5.22 以降安定): 先頭アイテムが actions ワードで、
    /// 7 bit ごとに 1 アクション。各アクションは低 4 bit が base 種別、
    /// 0x30 が添字種別、0x40 が最終要素フラグ。base/添字の種別に応じて
    /// 後続アイテム (pad_offset / sv / iv) を消費する。
    /// MDEREF_* 定数は bindgen allowlist 対象外のためここにミラーする
    /// (libperl-sys への allowlist 追加候補)。
    fn decode_multideref(&self, aux: *mut libperl_sys::UNOP_AUX_item) -> OpDetail {
        const MDEREF_ACTION_MASK: u64 = 0xf;
        const MDEREF_INDEX_MASK: u64 = 0x30;
        const MDEREF_INDEX_CONST: u64 = 0x10;
        const MDEREF_INDEX_PADSV: u64 = 0x20;
        const MDEREF_INDEX_GVSV: u64 = 0x30;
        const MDEREF_FLAG_LAST: u64 = 0x40;
        const MDEREF_SHIFT: u32 = 7;

        if aux.is_null() {
            return OpDetail::Aux("multideref".into());
        }
        let mut steps: Vec<DerefStep> = Vec::new();
        unsafe {
            let mut items = aux;
            let mut actions = (*items).uv;
            // 暴走防止 (実際のチェーンは高々数段)
            while steps.len() < 64 {
                let kind = actions & MDEREF_ACTION_MASK;
                if kind == 0 {
                    // MDEREF_reload: 次のアイテムが新しい actions ワード
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
                    // 未知のアクション: 以降のアイテム境界が分からないので
                    // 打ち切り、デコード済みぶんだけ返す
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
                    _ => None, // INDEX_none: 添字は先行 op が計算
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

    /// UNOP_AUX アイテムから SV を取り出す。ithreads では SV アイテムは
    /// pad オフセットとして格納される (perl.h の UNOP_AUX_item_sv マクロ:
    /// `PAD_SVl((item)->pad_offset)`)。本プロジェクトは threaded perl
    /// 前提 (非 threaded では (*item).sv を直接読む形になる)
    unsafe fn aux_item_sv(&self, item: *const libperl_sys::UNOP_AUX_item) -> *const sv {
        PAD_BASE_SV(CvPADLIST(self.cv), unsafe { (*item).pad_offset })
    }

    /// pad index から変数名を引く (multideref の padsv 添字表示用)
    fn pad_name_at(&self, po: u64) -> Option<String> {
        let pnl = padlist_names(CvPADLIST(self.cv));
        if pnl.is_null() || po as isize > padnamelist_max(pnl) {
            return None;
        }
        let pn = padnamelist_nth(pnl, po as usize);
        if pn.is_null() {
            return None;
        }
        PadnamePV(pn).filter(|s| !s.is_empty())
    }

    /// pass 2: 生ポインタをノード id に解決
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

/// SV をリテラル値として写し取る (公式 API のみで構成)
fn sv_lit(sv: *const sv) -> SvLit {
    if sv.is_null() {
        return SvLit::Other("NULL".into());
    }
    let t = SvTYPE(sv);
    match t {
        svtype::SVt_PVCV => return SvLit::Code,
        svtype::SVt_REGEXP => return SvLit::Other("REGEXP".into()),
        svtype::SVt_PVGV | svtype::SVt_PVLV if isGV_with_GP(sv) => {
            return SvLit::Glob {
                name: HEK_KEY(GvNAME_HEK(sv)),
                stash: HvNAME(GvSTASH(sv)),
            };
        }
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
                // NUL を含みうるので SvCUR ぶんを読む
                let len = libperl_sys::SvCUR(sv);
                let bytes = std::slice::from_raw_parts(pv as *const u8, len as usize);
                SvLit::Pv(String::from_utf8_lossy(bytes).into_owned())
            }
        } else {
            SvLit::Undef
        }
    }
}

/// 名前付き pad エントリを収集 (targ → 名前/宣言型のテーブル)
fn capture_pad(cv: *const libperl_sys::cv) -> Vec<PadEntry> {
    let mut out = Vec::new();
    let pl = CvPADLIST(cv);
    if pl.is_null() {
        return out;
    }
    let pnl = padlist_names(pl);
    if pnl.is_null() {
        return out;
    }
    let max = padnamelist_max(pnl);
    for ix in 0..=max {
        if ix < 0 {
            continue;
        }
        let pn = padnamelist_nth(pnl, ix as usize);
        if pn.is_null() {
            continue;
        }
        let name = PadnamePV(pn);
        if name.as_deref().is_none_or(str::is_empty) {
            continue; // 無名スロット (一時領域・const 用) は載せない
        }
        out.push(PadEntry {
            ix: ix as u32,
            name,
            typ: PadnameTYPE(pn),
            flags: PadnameFLAGS(pn),
        });
    }
    out
}
