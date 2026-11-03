//! The evaluator: walks the OpNode tree (not `op_next`) of a sub body,
//! forking at symbolic conditions and joining after each compound
//! statement.

use std::sync::Arc;

use crate::ir::{OpDetail, OpNode, SubIr, SvLit, padname_flags};
use crate::render::render;

use super::builtins;
use super::env::{
    Env, Frame, hash_get, join_syms, list_concat, list_drop, list_elem, list_len,
    list_pop, list_shift, mk_list, to_hash, view,
};
use super::summary::{Assign, CallEvent, ForwardEvent, Precond, Read, ReadKind, Summary};
use super::value::{Cond, Constraint, HashSeg, Key, Norm, Sym};
use super::{IrSource, Limits};

const OPF_WANT: u8 = 3;
const OPF_WANT_LIST: u8 = 3;
const OPF_STACKED: u8 = 64;
const OPF_MOD: u8 = 32;
const OPP_LVAL_INTRO: u8 = 0x80;
const OPP_TARGET_MY: u8 = 0x10;
const OPP_MDEREF_EXISTS: u8 = 16;
const OPP_MDEREF_DELETE: u8 = 32;

/// Subs that never return (a call to one ends the path).
const DIE_SUBS: &[&str] = &["croak", "confess"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Want {
    Scalar,
    List,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dead {
    Die,
    Return,
}

/// Ops whose list-context value is a list (others are wrapped into a
/// one-item list).
fn returns_list(name: &str) -> bool {
    matches!(
        name,
        "rv2av"
            | "padav"
            | "rv2hv"
            | "padhv"
            | "list"
            | "sort"
            | "grepwhile"
            | "mapwhile"
            | "keys"
            | "values"
            | "reverse"
            | "split"
            | "splice"
            | "aslice"
            | "hslice"
            | "kvhslice"
            | "kvaslice"
            | "entersub"
            | "flop"
            | "range"
            | "caller"
            | "cond_expr"
            | "aassign"
            | "lineseq"
            | "scope"
            | "leave"
            | "leavetry"
            | "return"
    )
}

/// Ops that assign their result to their pad target under
/// `OPpTARGET_MY` (`$x = $a + $b` compiled into the `add`).
fn targlex(name: &str) -> bool {
    matches!(
        name,
        "add"
            | "subtract"
            | "multiply"
            | "divide"
            | "modulo"
            | "i_add"
            | "i_subtract"
            | "i_multiply"
            | "i_divide"
            | "i_modulo"
            | "concat"
            | "multiconcat"
            | "join"
            | "stringify"
            | "sprintf"
            | "length"
            | "substr"
            | "lc"
            | "uc"
    )
}

fn want_of(n: &OpNode) -> Want {
    if n.flags & OPF_WANT == OPF_WANT_LIST {
        Want::List
    } else {
        Want::Scalar
    }
}

fn is_die_sub(name: &str) -> bool {
    let last = name.rsplit("::").next().unwrap_or(name);
    DIE_SUBS.contains(&last) || name == "CORE::die" || name == "CORE::GLOBAL::die"
}

fn gv_of(n: &OpNode) -> Option<(&str, &str)> {
    match &n.detail {
        OpDetail::Gv { name, stash } => Some((name, stash.as_deref().unwrap_or("main"))),
        _ => None,
    }
}

fn is_args_gv(n: &OpNode) -> bool {
    gv_of(n) == Some(("_", "main"))
}

/// Where an array op operates.
enum ArrayTarget {
    Args,
    Pad(u64),
    Global(String),
    Value(Sym),
}

/// The callee of an entersub.
enum Callee<'a> {
    Named(String),
    Method { name: String, sup: bool },
    Dynamic(&'a OpNode),
}

pub(crate) struct Exec<'a> {
    ir: &'a SubIr,
    src: &'a dyn IrSource,
    limits: &'a Limits,
    steps: u32,
    env: Env,
    line: Option<u32>,
    dead: Option<Dead>,
    loop_depth: u32,
    seq: u32,
    ret_envs: Vec<Env>,
    sum: Summary,
    /// Bodies of the inlined calls in progress (the IR of the top frame).
    ir_stack: Vec<Arc<SubIr>>,
    /// Values and states at the `return`s of each inlined call in progress.
    inline_rets: Vec<Vec<(Sym, Env)>>,
    /// Number of calls inlined so far.
    inlines: u32,
    /// Line of the analyzed sub's call being inlined (what writes through
    /// aliases are recorded at).
    top_call_line: Option<u32>,
}

impl<'a> Exec<'a> {
    pub(crate) fn new(ir: &'a SubIr, src: &'a dyn IrSource, limits: &'a Limits) -> Exec<'a> {
        let env = Env::new("");
        Exec {
            ir,
            src,
            limits,
            steps: 0,
            env: env.clone(),
            line: None,
            dead: None,
            loop_depth: 0,
            seq: 0,
            ret_envs: Vec::new(),
            ir_stack: Vec::new(),
            inline_rets: Vec::new(),
            inlines: 0,
            top_call_line: None,
            sum: Summary {
                env,
                reads: Vec::new(),
                assigns: Vec::new(),
                preconds: Vec::new(),
                writes_to_caller: Vec::new(),
                calls: Vec::new(),
                forwards: Vec::new(),
                returns: Vec::new(),
                unknowns: Vec::new(),
                notes: Vec::new(),
                exhausted: false,
            },
        }
    }

    /// Execute the body and return the summary.
    pub(crate) fn run(mut self) -> Summary {
        if let Some(body) = crate::passes::body(self.ir) {
            let v = self.eval_block(body, Want::List);
            if self.dead.is_none() && crate::passes::has_implicit_value(self.ir) {
                self.sum.returns.push(v);
            }
        }
        let width = self.limits.join_width;
        let mut fin: Option<Env> = if self.dead.is_none() {
            Some(self.env.clone())
        } else {
            None
        };
        for e in std::mem::take(&mut self.ret_envs) {
            fin = Some(match fin {
                None => e,
                Some(a) => Env::join(a, e, &[], &[], width),
            });
        }
        self.sum.env = fin.unwrap_or(self.env);
        self.sum
    }

    // ---- bookkeeping ----

    /// The IR of the sub being executed (an inlined callee or the
    /// analyzed sub).
    fn ir(&self) -> &SubIr {
        self.ir_stack.last().map_or(self.ir, |a| a)
    }

    /// Whether execution is in the analyzed sub itself (not in an inlined
    /// callee): only its reads, assignments and checks are recorded.
    fn at_top(&self) -> bool {
        self.env.frames.len() == 1
    }

    fn next_seq(&mut self) -> u32 {
        self.seq += 1;
        self.seq
    }

    fn conditional(&self) -> bool {
        !self.env.conds.is_empty() || self.loop_depth > 0
    }

    fn note_read(&mut self, v: &Sym, kind: ReadKind, via: Option<String>, text: Option<String>) {
        if !v.mentions_args() || !self.at_top() {
            return;
        }
        let seq = self.next_seq();
        self.sum.reads.push(Read {
            seq,
            sym: v.clone(),
            line: self.line,
            conditional: self.conditional(),
            kind,
            via,
            text,
        });
    }

    fn unknown(&mut self, op: &str) {
        self.sum.unknowns.push((op.to_string(), self.line));
    }

    fn note(&mut self, s: String) {
        if !self.sum.notes.contains(&s) {
            self.sum.notes.push(s);
        }
    }

    // ---- pads ----

    fn pad_entry(&self, targ: u64) -> Option<&crate::ir::PadEntry> {
        self.ir().pad.iter().find(|e| e.ix as u64 == targ)
    }

    fn pad_name(&self, targ: u64) -> Option<String> {
        self.ir().pad_name(targ).map(String::from)
    }

    fn pad_default(&self, targ: u64) -> Sym {
        match self.pad_entry(targ) {
            Some(e) if e.flags & padname_flags::OUTER != 0 => Sym::Unknown("outer"),
            Some(e) => match e.name.as_deref().and_then(|n| n.chars().next()) {
                Some('@') => Sym::empty(),
                Some('%') => Sym::Hash { segs: Vec::new() },
                _ => Sym::undef(),
            },
            None => Sym::undef(),
        }
    }

    fn pad_get(&self, targ: u64) -> Sym {
        if let Some(v) = self.env.top().pads.get(&targ) {
            return v.clone();
        }
        match self.pad_entry(targ) {
            Some(e) if e.flags & padname_flags::OUTER != 0 => self
                .src
                .outer_value(&self.env.top().callee, targ as u32)
                .unwrap_or(Sym::Unknown("outer")),
            _ => self.pad_default(targ),
        }
    }

    /// The IR of frame `frame` (0 = the analyzed sub).
    fn frame_ir(&self, frame: u32) -> &SubIr {
        match frame {
            0 => self.ir,
            k => self.ir_stack.get(k as usize - 1).map_or(self.ir, |a| a),
        }
    }

    /// The value behind an alias of a caller's variable.
    fn resolve(&self, v: Sym) -> Sym {
        match v {
            Sym::Alias { frame, targ } => self
                .env
                .frames
                .get(frame as usize)
                .and_then(|f| f.pads.get(&targ))
                .cloned()
                .unwrap_or_else(Sym::undef),
            Sym::Join(alts) => Sym::Join(alts.into_iter().map(|(c, s)| (c, self.resolve(s))).collect()),
            v => v,
        }
    }

    fn resolve_list(&self, l: Sym) -> Sym {
        match l {
            Sym::List { items, tail } => Sym::List {
                items: items.into_iter().map(|i| self.resolve(i)).collect(),
                tail,
            },
            Sym::Join(alts) => Sym::Join(alts.into_iter().map(|(c, s)| (c, self.resolve_list(s))).collect()),
            l => l,
        }
    }

    /// The element of the current `@_` that an element op (`$_[0]`,
    /// `$_[$i]`) names, without resolving aliases; None for other ops.
    fn raw_arg_elem(&mut self, n: &OpNode) -> Option<Sym> {
        let ix: i64 = match n.name.as_str() {
            "aelemfast" if is_args_gv(n) => n.private as i8 as i64,
            "aelem" => {
                let a = n.kids.first()?.skip_null();
                let is_args = a.name == "rv2av" && a.kids.first().is_some_and(|k| is_args_gv(k.skip_null()));
                if !is_args {
                    return None;
                }
                let i = self.eval(n.kids.get(1)?, Want::Scalar);
                i.const_int()?
            }
            "multideref" => {
                let OpDetail::MultiDeref { steps } = &n.detail else {
                    return None;
                };
                let [st] = steps.as_slice() else {
                    return None;
                };
                if st.base != "gvav" || st.base_name.as_deref() != Some("_") {
                    return None;
                }
                match self.mderef_key(st.key.as_deref(), st.key_targ, true) {
                    Key::Index(i) => i,
                    _ => return None,
                }
            }
            _ => return None,
        };
        Some(list_elem(&self.env.top().args.clone(), ix))
    }

    fn pad_set(&mut self, targ: u64, v: Sym, source: &'static str) {
        self.env.top_mut().pads.insert(targ, v.clone());
        if self.at_top() {
            self.record_assign(targ, v, source);
        }
    }

    /// Record the first argument-derived assignment to a pad entry of the
    /// analyzed sub.
    fn record_assign(&mut self, targ: u64, v: Sym, source: &'static str) {
        if v.mentions_args() && !self.sum.assigns.iter().any(|a| a.targ == targ) {
            let seq = self.next_seq();
            let line = if self.at_top() { self.line } else { self.top_call_line };
            self.sum.assigns.push(Assign {
                seq,
                targ,
                sym: v,
                line,
                conditional: self.conditional(),
                source,
            });
        }
    }

    /// Pad entry by name (multideref keys carry names, not targs);
    /// ambiguous names give None.
    fn pad_by_name(&self, name: &str) -> Option<u64> {
        let mut it = self
            .ir()
            .pad
            .iter()
            .filter(|e| e.name.as_deref() == Some(name));
        let first = it.next()?;
        it.next().is_none().then_some(first.ix as u64)
    }

    // ---- evaluation ----

    fn eval_block(&mut self, n: &OpNode, want: Want) -> Sym {
        let mut last = Sym::empty();
        let count = n.kids.len();
        for (i, k) in n.kids.iter().enumerate() {
            if self.dead.is_some() || self.sum.exhausted {
                break;
            }
            if let OpDetail::Cop { line, .. } = &k.detail {
                self.line = Some(*line);
                continue;
            }
            if matches!(k.name.as_str(), "enter" | "entertry" | "unstack" | "pushmark") {
                continue;
            }
            let w = if i + 1 == count { want } else { Want::Scalar };
            last = self.eval(k, w);
        }
        last
    }

    pub(crate) fn eval(&mut self, n: &OpNode, want: Want) -> Sym {
        if self.dead.is_some() {
            return Sym::Unknown("dead");
        }
        self.steps += 1;
        if self.steps > self.limits.steps {
            if !self.sum.exhausted {
                self.sum.exhausted = true;
                self.note("step budget exhausted".into());
            }
            return Sym::Unknown("budget");
        }
        if let OpDetail::Cop { line, .. } = &n.detail {
            self.line = Some(*line);
            return Sym::empty();
        }
        if n.op_type == 0 {
            return self.eval_null(n, want);
        }
        let v = self.eval_op(n, want);
        if want == Want::List && !returns_list(&n.name) {
            Sym::List {
                items: vec![v],
                tail: None,
            }
        } else {
            v
        }
    }

    fn eval_null(&mut self, n: &OpNode, want: Want) -> Sym {
        match n.kids.len() {
            0 => {
                if want == Want::List {
                    Sym::empty()
                } else {
                    Sym::Unknown("null")
                }
            }
            1 => self.eval(&n.kids[0], want),
            _ => match n.was.as_deref() {
                Some("aelem" | "helem" | "exists" | "delete" | "rv2av" | "rv2hv" | "rv2sv") => {
                    self.eval(&n.kids[0], want)
                }
                _ => self.eval_list(&n.kids, want),
            },
        }
    }

    /// Kids as a comma list: concatenated in list context, the last value
    /// in scalar context.
    fn eval_list(&mut self, kids: &[OpNode], want: Want) -> Sym {
        let mut parts = Vec::new();
        let mut last = Sym::undef();
        for k in kids {
            if matches!(k.name.as_str(), "pushmark" | "padrange") {
                continue;
            }
            if k.op_type == 0 && k.kids.is_empty() && !matches!(k.detail, OpDetail::Cop { .. }) {
                continue;
            }
            match want {
                Want::List => parts.push(self.eval(k, Want::List)),
                Want::Scalar => last = self.eval(k, Want::Scalar),
            }
        }
        match want {
            Want::List => list_concat(parts),
            Want::Scalar => last,
        }
    }

    fn eval_kids_generic(&mut self, n: &OpNode) -> Vec<Sym> {
        let mut out = Vec::new();
        for k in &n.kids {
            if matches!(k.name.as_str(), "pushmark" | "padrange") {
                continue;
            }
            out.push(self.eval(k, want_of(k)));
        }
        out
    }

    fn eval_op(&mut self, n: &OpNode, want: Want) -> Sym {
        let name = n.name.as_str();
        match name {
            "lineseq" | "scope" | "leave" => self.eval_block(n, want),
            "nextstate" | "dbstate" | "pushmark" | "padrange" | "enter" | "unstack" | "iter"
            | "next" | "last" | "redo" => Sym::empty(),
            "const" => match &n.detail {
                OpDetail::Const(SvLit::Code) => Sym::Code(Some(n.id)),
                OpDetail::Const(lit) => Sym::Const(lit.clone()),
                _ => Sym::Unknown("const"),
            },
            "anoncode" => Sym::Code(Some(n.id)),
            "padsv" => {
                if n.private & OPP_LVAL_INTRO != 0 {
                    self.env.top_mut().pads.insert(n.targ, Sym::undef());
                }
                self.pad_get(n.targ)
            }
            "padav" | "padhv" => {
                if n.private & OPP_LVAL_INTRO != 0 {
                    let d = self.pad_default(n.targ);
                    self.env.top_mut().pads.insert(n.targ, d);
                }
                let v = self.pad_get(n.targ);
                match want {
                    Want::List => v,
                    Want::Scalar if name == "padav" => list_len(&v),
                    Want::Scalar => Sym::Len(Box::new(v)),
                }
            }
            "padsv_store" => {
                let before = self.inlines;
                let v = match n.kids.first() {
                    Some(k) => self.eval(k, Want::Scalar),
                    None => Sym::Unknown("padsv_store"),
                };
                let src = if self.inlines > before {
                    "helper"
                } else {
                    n.kids.first().map_or("expr", source_of)
                };
                self.pad_set(n.targ, v.clone(), src);
                v
            }
            "sassign" => {
                let before = self.inlines;
                let v = match n.kids.first() {
                    Some(k) => self.eval(k, Want::Scalar),
                    None => Sym::Unknown("sassign"),
                };
                if let (Some(lhs), Some(rhs)) = (n.kids.get(1), n.kids.first()) {
                    let src = if self.inlines > before { "helper" } else { source_of(rhs) };
                    self.assign(lhs, v.clone(), src);
                }
                v
            }
            "aassign" => {
                let before = self.inlines;
                let rhs = match n.kids.first() {
                    Some(k) => self.eval(k, Want::List),
                    None => Sym::empty(),
                };
                if let Some(lhs) = n.kids.get(1) {
                    let src = match (rhs.mentions_args(), self.inlines > before) {
                        (true, true) => "helper",
                        (true, false) => "unpack",
                        _ => "expr",
                    };
                    self.assign_list(lhs, rhs.clone(), src);
                }
                match want {
                    Want::List => rhs,
                    Want::Scalar => list_len(&rhs),
                }
            }
            "rv2av" => {
                let t = self.array_target(n);
                let v = self.resolve_list(self.array_get(&t));
                match want {
                    Want::List => v,
                    Want::Scalar => list_len(&v),
                }
            }
            "rv2hv" => {
                let v = match n.kids.first().map(|k| k.skip_null()) {
                    Some(k) if gv_of(k).is_some() => {
                        let (nm, st) = gv_of(k).unwrap();
                        self.global(&format!("%{}::{}", st, nm))
                    }
                    Some(k) => {
                        let r = self.eval(k, Want::Scalar);
                        self.deref(r, '%')
                    }
                    None => Sym::Unknown("rv2hv"),
                };
                match want {
                    Want::List => v,
                    Want::Scalar => Sym::Len(Box::new(v)),
                }
            }
            "rv2sv" => match n.kids.first().map(|k| k.skip_null()) {
                Some(k) if gv_of(k).is_some() => {
                    let (nm, st) = gv_of(k).unwrap();
                    self.global(&format!("${}::{}", st, nm))
                }
                Some(k) => {
                    let r = self.eval(k, Want::Scalar);
                    self.deref(r, '$')
                }
                None => Sym::Unknown("rv2sv"),
            },
            "gvsv" => match gv_of(n) {
                Some((nm, st)) => self.global(&format!("${}::{}", st, nm)),
                None => Sym::Unknown("gvsv"),
            },
            "shift" | "pop" => {
                let t = match n.kids.first() {
                    None => ArrayTarget::Args,
                    Some(k) => self.array_target(k.skip_null()),
                };
                let cur = self.array_get(&t);
                let (v, rest) = if name == "shift" {
                    list_shift(&cur)
                } else {
                    list_pop(&cur)
                };
                self.array_set(&t, rest);
                self.resolve(v)
            }
            "splice" => self.eval_splice(n, want),
            "aelemfast" => {
                let ix = n.private as i8 as i64;
                if is_args_gv(n) {
                    let v = self.resolve(list_elem(&self.env.top().args.clone(), ix));
                    let text = render(self.ir(), n);
                    self.note_read(&v, ReadKind::Elem, None, Some(text));
                    v
                } else {
                    match gv_of(n) {
                        Some((nm, st)) => {
                            let a = self.global(&format!("@{}::{}", st, nm));
                            list_elem(&a, ix)
                        }
                        None => Sym::Unknown("aelemfast"),
                    }
                }
            }
            "aelemfast_lex" => {
                let a = self.pad_get(n.targ);
                list_elem(&a, n.private as i8 as i64)
            }
            "aelem" => {
                let (Some(ak), Some(ik)) = (n.kids.first(), n.kids.get(1)) else {
                    return Sym::Unknown("aelem");
                };
                let t = self.array_target(ak.skip_null());
                let a = self.array_get(&t);
                let ix = self.eval(ik, Want::Scalar);
                let v = match ix.const_int() {
                    Some(i) => self.resolve(list_elem(&a, i)),
                    None => Sym::Elem(Box::new(a), Key::Sym(Box::new(ix))),
                };
                if matches!(t, ArrayTarget::Args) {
                    let text = render(self.ir(), n);
                    self.note_read(&v, ReadKind::Elem, None, Some(text));
                }
                v
            }
            "helem" => {
                let (Some(hk), Some(kk)) = (n.kids.first(), n.kids.get(1)) else {
                    return Sym::Unknown("helem");
                };
                let h = self.eval(hk, Want::List);
                let via = pad_via(self.ir(), hk);
                let k = self.eval(kk, Want::Scalar);
                let key = key_of(k, false);
                self.lookup(&h, &key, via, n)
            }
            "multideref" => self.eval_mderef(n),
            "exists" | "delete" => {
                let v = match n.kids.first() {
                    Some(k) => self.eval(k, Want::Scalar),
                    None => Sym::Unknown("exists"),
                };
                if name == "exists" {
                    builtins::apply("exists", vec![v])
                } else {
                    v
                }
            }
            // the operands are scalars; `eval` wraps the value in list context
            "and" | "or" | "dor" => self.eval_logop(n, Want::Scalar),
            "andassign" | "orassign" | "dorassign" => self.eval_logassign(n),
            "cond_expr" => {
                let (Some(c), Some(t), Some(f)) = (n.kids.first(), n.kids.get(1), n.kids.get(2))
                else {
                    return Sym::Unknown("cond_expr");
                };
                let cv = self.eval(c, Want::Scalar);
                self.fork(cv, |x| x.eval(t, want), |x| x.eval(f, want))
            }
            "entersub" => self.eval_call(n, want),
            "aslice" => self.eval_aslice(n, want),
            "goto" => self.eval_goto(n),
            "die" => {
                self.eval_kids_generic(n);
                self.dead = Some(Dead::Die);
                Sym::Unknown("die")
            }
            "return" => {
                let v = self.eval_list(&n.kids, Want::List);
                if self.dead.is_none() {
                    self.leave_with(v.clone());
                }
                v
            }
            "leavetry" => {
                let v = self.eval_block(n, want);
                // a die is caught; a return leaves only the eval
                self.dead = None;
                v
            }
            "leaveloop" => {
                self.eval_loop(n);
                Sym::empty()
            }
            "anonlist" => {
                let l = self.eval_list(&n.kids, Want::List);
                Sym::Ref(Box::new(l))
            }
            "anonhash" => {
                let l = self.eval_list(&n.kids, Want::List);
                Sym::Ref(Box::new(to_hash(&l)))
            }
            "emptyavhv" => Sym::Ref(Box::new(Sym::empty())),
            "srefgen" | "refgen" => {
                let k = n.kids.first().map(lvalue_core);
                if let Some(Sym::Alias { frame, targ }) = k.and_then(|k| self.raw_arg_elem(k)) {
                    return Sym::RefPad { frame, targ };
                }
                match k {
                    Some(k) if k.name == "padsv" => Sym::RefPad {
                        frame: (self.env.frames.len() - 1) as u32,
                        targ: k.targ,
                    },
                    Some(k) => {
                        let v = self.eval(k, Want::List);
                        Sym::Ref(Box::new(v))
                    }
                    None => Sym::Unknown("refgen"),
                }
            }
            "keys" => {
                let h = match n.kids.first() {
                    Some(k) => self.eval(k, Want::List),
                    None => Sym::Unknown("keys"),
                };
                let v = match &h {
                    Sym::Hash { segs } if builtins::is_concrete_hash(&h) => {
                        let mut ks: Vec<String> = Vec::new();
                        for s in segs {
                            if let HashSeg::Pairs(p) = s {
                                for (k, _) in p {
                                    if !ks.contains(k) {
                                        ks.push(k.clone());
                                    }
                                }
                            }
                        }
                        mk_list(ks.iter().map(|k| Sym::pv(k)).collect(), None)
                    }
                    _ => Sym::Keys(Box::new(h)),
                };
                match want {
                    Want::List => v,
                    Want::Scalar => list_len(&v),
                }
            }
            "sort" => {
                // the comparison block (OPf_STACKED) is not evaluated
                let skip = if n.flags & OPF_STACKED != 0 { 2 } else { 1 };
                let rest: Vec<OpNode> = n.kids.iter().skip(skip).cloned().collect();
                let l = self.eval_list(&rest, Want::List);
                match want {
                    Want::List => l,
                    Want::Scalar => Sym::Unknown("sort"),
                }
            }
            "reverse" => {
                let l = self.eval_list(&n.kids, Want::List);
                match (want, view(&l)) {
                    (Want::List, (mut items, None)) => {
                        items.reverse();
                        mk_list(items, None)
                    }
                    (Want::List, _) => Sym::Unknown("reverse"),
                    (Want::Scalar, _) => Sym::Unknown("reverse"),
                }
            }
            "grepwhile" | "mapwhile" => {
                let Some(start) = n.kids.first().map(|k| k.skip_null()) else {
                    return Sym::Unknown("grep");
                };
                // kids of grepstart: pushmark, the block / expr (not
                // evaluated), the list
                let list: Vec<OpNode> = start
                    .kids
                    .iter()
                    .filter(|k| k.name != "pushmark")
                    .skip(1)
                    .cloned()
                    .collect();
                let l = self.eval_list(&list, Want::List);
                let v = if name == "grepwhile" {
                    mk_list(Vec::new(), Some(Sym::App("grep", vec![l])))
                } else {
                    Sym::Unknown("map")
                };
                match want {
                    Want::List => v,
                    Want::Scalar => Sym::Len(Box::new(v)),
                }
            }
            "push" | "unshift" => {
                let mut kids = n.kids.iter().filter(|k| k.name != "pushmark");
                let Some(ak) = kids.next() else {
                    return Sym::Unknown("push");
                };
                let t = self.array_target(ak.skip_null());
                let rest: Vec<OpNode> = kids.cloned().collect();
                let items = self.eval_list(&rest, Want::List);
                let cur = self.array_get(&t);
                let new = if name == "push" {
                    list_concat(vec![cur, items])
                } else {
                    list_concat(vec![items, cur])
                };
                let len = list_len(&new);
                self.array_set(&t, new);
                len
            }
            "preinc" | "postinc" | "predec" | "postdec" | "i_preinc" | "i_postinc"
            | "i_predec" | "i_postdec" => {
                let Some(k) = n.kids.first() else {
                    return Sym::Unknown("inc");
                };
                let cur = self.eval(k, Want::Scalar);
                let op = if name.contains("inc") { "+" } else { "-" };
                let new = builtins::apply(op, vec![cur.clone(), Sym::iv(1)]);
                self.assign(k, new.clone(), "expr");
                if name.contains("post") { cur } else { new }
            }
            "undef" => {
                if let Some(k) = n.kids.first() {
                    let d = match k.skip_null().name.as_str() {
                        "padav" | "rv2av" => Sym::empty(),
                        "padhv" | "rv2hv" => Sym::Hash { segs: Vec::new() },
                        _ => Sym::undef(),
                    };
                    self.assign(k, d, "expr");
                }
                Sym::undef()
            }
            "multiconcat" => {
                let pieces = match &n.detail {
                    OpDetail::MultiConcat { pieces } => pieces.clone(),
                    _ => Vec::new(),
                };
                let operands: Vec<Sym> = n
                    .kids
                    .iter()
                    .filter(|k| k.name != "pushmark")
                    .map(|k| self.eval(k, Want::Scalar))
                    .collect::<Vec<_>>();
                let mut parts = Vec::new();
                for (i, p) in pieces.iter().enumerate() {
                    if let Some(s) = p {
                        parts.push(Sym::pv(s));
                    }
                    if let Some(o) = operands.get(i) {
                        parts.push(o.clone());
                    }
                }
                let v = builtins::apply(".", parts);
                self.store_result(n, v)
            }
            "list" => self.eval_list(&n.kids, want),
            "scalar" => match n.kids.first() {
                Some(k) => self.eval(k, Want::Scalar),
                None => Sym::Unknown("scalar"),
            },
            "av2arylen" => {
                let t = match n.kids.first() {
                    Some(k) => self.array_target(k.skip_null()),
                    None => return Sym::Unknown("av2arylen"),
                };
                let a = self.array_get(&t);
                builtins::apply("-", vec![list_len(&a), Sym::iv(1)])
            }
            "wantarray" => Sym::Unknown("wantarray"),
            "caller" => {
                // `caller(N)` (from package DB) sets @DB::args to the
                // arguments of the N-th caller up
                let lvl = n
                    .kids
                    .first()
                    .map(|k| self.eval(k, Want::Scalar))
                    .and_then(|v| v.const_int());
                if let Some(lvl) = lvl {
                    let len = self.env.frames.len() as i64;
                    if lvl >= 1 && lvl < len {
                        let a = self.env.frames[(len - 1 - lvl) as usize].args.clone();
                        let a = self.resolve_list(a);
                        self.env.globals.insert("@DB::args".into(), a);
                    }
                }
                Sym::Unknown("caller")
            }
            "subst" => self.eval_subst(n),
            _ => {
                if let Some(sym) = builtins::op_symbol(name) {
                    let args = self.eval_kids_generic(n);
                    let args = if args.is_empty() && matches!(sym, "defined" | "ref" | "lc" | "uc" | "length") {
                        vec![self.global("$main::_")]
                    } else {
                        args
                    };
                    let v = builtins::apply(sym, args);
                    return self.store_result(n, v);
                }
                self.eval_kids_generic(n);
                self.unknown(name);
                Sym::Unknown("op")
            }
        }
    }

    /// Assign the result of an op to its target: the first kid under
    /// `OPf_STACKED` (`$x += 1`), the pad target under `OPpTARGET_MY`.
    fn store_result(&mut self, n: &OpNode, v: Sym) -> Sym {
        if n.flags & OPF_STACKED != 0 {
            if let Some(k) = n.kids.first() {
                self.assign(k, v.clone(), "expr");
            }
        } else if targlex(&n.name) && n.private & OPP_TARGET_MY != 0 && n.targ != 0 {
            self.pad_set(n.targ, v.clone(), "expr");
        }
        v
    }

    fn global(&self, name: &str) -> Sym {
        self.env
            .globals
            .get(name)
            .cloned()
            .unwrap_or(Sym::Unknown("global"))
    }

    fn deref(&self, r: Sym, sigil: char) -> Sym {
        match r {
            Sym::Join(alts) => Sym::Join(
                alts.into_iter()
                    .map(|(c, s)| (c, self.deref(s, sigil)))
                    .collect(),
            ),
            Sym::Ref(x) => *x,
            Sym::RefPad { targ, .. } => self.pad_get(targ),
            Sym::Unknown(_) => r,
            other => Sym::Deref(Box::new(other), sigil),
        }
    }

    /// `$x =~ s/^LIT//`: the target becomes `strip_prefix($x, LIT)`;
    /// other substitutions make it unknown.
    fn eval_subst(&mut self, n: &OpNode) -> Sym {
        let target: Option<&OpNode> = if n.flags & OPF_STACKED != 0 {
            n.kids.first()
        } else {
            None
        };
        let repl: Vec<&OpNode> = n
            .kids
            .iter()
            .skip(usize::from(target.is_some()))
            .filter(|k| !(k.op_type == 0 && k.kids.is_empty()))
            .collect();
        let prefix = match &n.detail {
            OpDetail::Pm {
                pattern: Some(p),
                flags: None,
            } => literal_prefix(p),
            _ => None,
        };
        let empty_repl = match repl.as_slice() {
            [] => true,
            [k] => matches!(&k.skip_null().detail, OpDetail::Const(SvLit::Pv(s)) if s.is_empty()),
            _ => false,
        };
        let cur = match target {
            Some(t) => self.eval(t, Want::Scalar),
            None if n.targ != 0 => self.pad_get(n.targ),
            None => self.global("$main::_"),
        };
        let new = match (prefix, empty_repl) {
            (Some(p), true) => match cur.const_str() {
                Some(s) => Sym::pv(s.strip_prefix(p.as_str()).unwrap_or(&s)),
                None => Sym::App("strip_prefix", vec![cur, Sym::pv(&p)]),
            },
            _ => Sym::App("subst", vec![cur]),
        };
        match target {
            Some(t) => self.assign(t, new, "expr"),
            None if n.targ != 0 => self.pad_set(n.targ, new, "expr"),
            None => {
                self.env.globals.insert("$main::_".into(), new);
            }
        }
        Sym::Unknown("subst")
    }

    // ---- arrays ----

    fn array_target(&mut self, n: &OpNode) -> ArrayTarget {
        match n.name.as_str() {
            "padav" => ArrayTarget::Pad(n.targ),
            "rv2av" => match n.kids.first().map(|k| k.skip_null()) {
                Some(k) if is_args_gv(k) => ArrayTarget::Args,
                Some(k) if gv_of(k).is_some() => {
                    let (nm, st) = gv_of(k).unwrap();
                    ArrayTarget::Global(format!("@{}::{}", st, nm))
                }
                Some(k) => {
                    let r = self.eval(k, Want::Scalar);
                    ArrayTarget::Value(self.deref(r, '@'))
                }
                None => ArrayTarget::Value(Sym::Unknown("rv2av")),
            },
            "gv" if is_args_gv(n) => ArrayTarget::Args,
            _ => {
                let v = self.eval(n, Want::List);
                ArrayTarget::Value(v)
            }
        }
    }

    fn array_get(&self, t: &ArrayTarget) -> Sym {
        match t {
            ArrayTarget::Args => self.env.top().args.clone(),
            ArrayTarget::Pad(targ) => self.pad_get(*targ),
            ArrayTarget::Global(g) => self.global(g),
            ArrayTarget::Value(v) => v.clone(),
        }
    }

    fn array_set(&mut self, t: &ArrayTarget, v: Sym) {
        match t {
            ArrayTarget::Args => self.env.top_mut().args = v,
            ArrayTarget::Pad(targ) => {
                self.env.top_mut().pads.insert(*targ, v);
            }
            ArrayTarget::Global(g) => {
                self.env.globals.insert(g.clone(), v);
            }
            ArrayTarget::Value(_) => {}
        }
    }

    fn eval_splice(&mut self, n: &OpNode, want: Want) -> Sym {
        let kids: Vec<&OpNode> = n.kids.iter().filter(|k| k.name != "pushmark").collect();
        let Some(ak) = kids.first() else {
            return Sym::Unknown("splice");
        };
        let t = self.array_target(ak.skip_null());
        let rest: Vec<Sym> = kids[1..].iter().map(|k| self.eval(k, Want::Scalar)).collect();
        let cur = self.array_get(&t);
        let off = rest.first().map(|s| s.const_int());
        let len = rest.get(1).map(|s| s.const_int());
        let (removed, remaining) = match (off, len, rest.len()) {
            (None, _, 0) => (cur.clone(), Sym::empty()),
            (Some(Some(0)), Some(Some(l)), 2) if l >= 0 => {
                let mut items = Vec::new();
                let mut c = cur;
                for _ in 0..l {
                    let (v, r) = list_shift(&c);
                    items.push(v);
                    c = r;
                }
                (mk_list(items, None), c)
            }
            _ => {
                self.array_set(&t, Sym::Unknown("splice"));
                return Sym::Unknown("splice");
            }
        };
        self.array_set(&t, remaining);
        match want {
            Want::List => removed,
            Want::Scalar => match view(&removed) {
                (items, None) => items.last().cloned().unwrap_or_else(Sym::undef),
                _ => Sym::Unknown("splice"),
            },
        }
    }

    // ---- hashes and multideref ----

    /// Hash lookup with read bookkeeping.
    fn lookup(&mut self, h: &Sym, key: &Key, via: Option<String>, n: &OpNode) -> Sym {
        let mut defaults: Vec<(Sym, Sym)> = Vec::new();
        let v = hash_get(h, key, &mut |arg, d| defaults.push((arg.clone(), d.clone())));
        let text = Some(render(self.ir(), n));
        for (arg, d) in defaults {
            self.note_read(&arg, ReadKind::Default(d), via.clone(), text.clone());
        }
        self.note_read(&v, ReadKind::Key, via, text);
        v
    }

    fn mderef_key(&self, key: Option<&str>, key_targ: Option<u64>, array: bool) -> Key {
        if let Some(targ) = key_targ {
            return key_of(self.pad_get(targ), array);
        }
        let Some(k) = key else {
            return Key::Sym(Box::new(Sym::Unknown("key")));
        };
        if array {
            if let Ok(i) = k.parse::<i64>() {
                return Key::Index(i);
            }
        }
        if k.starts_with('$') {
            if let Some(targ) = self.pad_by_name(k) {
                return key_of(self.pad_get(targ), array);
            }
            // a package variable (`$_`)
            let name = k.strip_prefix('$').unwrap_or(k);
            let full = if name.contains("::") {
                format!("${}", name)
            } else {
                format!("$main::{}", name)
            };
            return key_of(self.global(&full), array);
        }
        if array {
            Key::Sym(Box::new(Sym::Unknown("index")))
        } else {
            Key::Str(k.to_string())
        }
    }

    fn eval_mderef(&mut self, n: &OpNode) -> Sym {
        let OpDetail::MultiDeref { steps } = &n.detail else {
            self.eval_kids_generic(n);
            return Sym::Unknown("multideref");
        };
        let mut cur: Option<Sym> = None;
        let mut via: Option<String> = None;
        let mut args_base = false;
        let text = render(self.ir(), n);
        let mut last_kind = ReadKind::Key;
        for st in steps {
            let array = st.container == "array";
            let container = match st.base.as_str() {
                "padav" | "padhv" => {
                    let targ = st.base_targ.unwrap_or(0);
                    via = self.pad_name(targ);
                    self.pad_get(targ)
                }
                "padsv" => {
                    let targ = st.base_targ.unwrap_or(0);
                    via = self.pad_name(targ);
                    let r = self.pad_get(targ);
                    self.deref_container(r, array)
                }
                "gvav" if st.base_name.as_deref() == Some("_") => {
                    args_base = true;
                    self.env.top().args.clone()
                }
                "gvav" | "gvhv" => {
                    let sigil = if array { '@' } else { '%' };
                    let nm = st.base_name.clone().unwrap_or_default();
                    let full = if nm.contains("::") {
                        format!("{}{}", sigil, nm)
                    } else {
                        format!("{}main::{}", sigil, nm)
                    };
                    self.global(&full)
                }
                "gvsv" => {
                    let nm = st.base_name.clone().unwrap_or_default();
                    let r = self.global(&format!("$main::{}", nm));
                    self.deref_container(r, array)
                }
                "chain" => {
                    let r = cur.take().unwrap_or(Sym::Unknown("chain"));
                    self.deref_container(r, array)
                }
                _ => {
                    // "stack": the value of the multideref's kid
                    let r = match n.kids.iter().find(|k| !(k.op_type == 0 && k.kids.is_empty())) {
                        Some(k) => self.eval(k, Want::Scalar),
                        None => Sym::Unknown("stack"),
                    };
                    self.deref_container(r, array)
                }
            };
            let key = self.mderef_key(st.key.as_deref(), st.key_targ, array);
            let v = if array {
                match &key {
                    Key::Index(i) => self.resolve(list_elem(&container, *i)),
                    k => Sym::Elem(Box::new(container), k.clone()),
                }
            } else if n.private & OPP_MDEREF_EXISTS != 0 && builtins::is_concrete_hash(&container) {
                // exists on a hash whose keys are all known
                if let (Key::Str(k), Sym::Hash { segs }) = (&key, &container) {
                    let found = segs.iter().any(|s| {
                        matches!(s, HashSeg::Pairs(p) if p.iter().any(|(pk, _)| pk == k))
                    });
                    return Sym::iv(i64::from(found));
                }
                Sym::Elem(Box::new(container), key.clone())
            } else {
                let mut defaults: Vec<(Sym, Sym)> = Vec::new();
                let v = hash_get(&container, &key, &mut |a, d| {
                    defaults.push((a.clone(), d.clone()))
                });
                for (a, d) in defaults {
                    self.note_read(&a, ReadKind::Default(d), via.clone(), Some(text.clone()));
                }
                v
            };
            last_kind = if array && args_base && cur.is_none() {
                ReadKind::Elem
            } else {
                ReadKind::Key
            };
            cur = Some(v);
        }
        let v = cur.unwrap_or(Sym::Unknown("multideref"));
        if n.private & OPP_MDEREF_EXISTS != 0 {
            self.note_read(&v, ReadKind::Exists, via, Some(text));
            return builtins::apply("exists", vec![v]);
        }
        let _ = n.private & OPP_MDEREF_DELETE;
        let via = if matches!(last_kind, ReadKind::Elem) { None } else { via };
        self.note_read(&v, last_kind, via, Some(text));
        v
    }

    /// The container a reference value points at, for an element access.
    fn deref_container(&self, r: Sym, array: bool) -> Sym {
        match r {
            Sym::Join(alts) => Sym::Join(
                alts.into_iter()
                    .map(|(c, s)| (c, self.deref_container(s, array)))
                    .collect(),
            ),
            Sym::Ref(x) => *x,
            Sym::RefPad { targ, .. } => self.pad_get(targ),
            Sym::Unknown(_) => r,
            other if array => Sym::Deref(Box::new(other), '@'),
            // hash_get wraps a non-hash value into Deref(_, '%') itself
            other => other,
        }
    }

    // ---- assignment ----

    fn assign(&mut self, lhs: &OpNode, v: Sym, source: &'static str) {
        let l = lvalue_core(lhs);
        if let Some(raw) = self.raw_arg_elem(l) {
            if let Sym::Alias { frame, targ } = raw {
                // an alias of the caller's variable: write through
                if let Some(f) = self.env.frames.get_mut(frame as usize) {
                    f.pads.insert(targ, v.clone());
                }
                if frame == 0 {
                    self.record_assign(targ, v, "helper");
                }
                return;
            }
        }
        match l.name.as_str() {
            "padsv" => self.pad_set(l.targ, v, source),
            "padav" => self.pad_set(l.targ, v, source),
            "padhv" => self.pad_set(l.targ, to_hash(&v), source),
            "padsv_store" => self.pad_set(l.targ, v, source),
            "rv2sv" | "gvsv" => {
                let gv = if l.name == "gvsv" {
                    gv_of(l)
                } else {
                    l.kids.first().and_then(|k| gv_of(k.skip_null()))
                };
                if let Some((nm, st)) = gv {
                    self.env.globals.insert(format!("${}::{}", st, nm), v);
                }
            }
            "rv2av" => {
                let t = self.array_target(l);
                self.array_set(&t, v);
            }
            "local" => {
                if let Some(k) = l.kids.first() {
                    self.assign(k, v, source);
                }
            }
            "helem" => {
                let (Some(hk), Some(kk)) = (l.kids.first(), l.kids.get(1)) else {
                    return;
                };
                let h = hk.skip_null();
                let k = self.eval(kk, Want::Scalar);
                if let (Some(k), "padhv") = (k.const_str(), h.name.as_str()) {
                    self.hash_store(h.targ, &k, v);
                }
            }
            "aelem" => {
                let (Some(ak), Some(ik)) = (l.kids.first(), l.kids.get(1)) else {
                    return;
                };
                let a = ak.skip_null();
                let i = self.eval(ik, Want::Scalar);
                if let (Some(i), "padav") = (i.const_int(), a.name.as_str()) {
                    self.array_store(a.targ, i, v);
                }
            }
            "aelemfast" if is_args_gv(l) => {
                let i = l.private as i8;
                self.note(format!("modifies caller arg {}", i));
                self.sum.writes_to_caller.push((format!("$_[{}]", i), v));
            }
            "multideref" => {
                let OpDetail::MultiDeref { steps } = &l.detail else {
                    return;
                };
                if let [st] = steps.as_slice() {
                    match (st.base.as_str(), st.base_name.as_deref(), st.key.as_deref()) {
                        ("gvav", Some("_"), Some(k)) => {
                            self.note(format!("modifies caller arg {}", k));
                            self.sum.writes_to_caller.push((format!("$_[{}]", k), v));
                        }
                        ("padhv", _, _) => {
                            let key = self.mderef_key(st.key.as_deref(), st.key_targ, false);
                            if let Key::Str(k) = key {
                                self.hash_store(st.base_targ.unwrap_or(0), &k, v);
                            }
                        }
                        ("padav", _, _) => {
                            let key = self.mderef_key(st.key.as_deref(), st.key_targ, true);
                            if let Key::Index(i) = key {
                                self.array_store(st.base_targ.unwrap_or(0), i, v);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// `$h{k} = v` on a pad hash.
    fn hash_store(&mut self, targ: u64, k: &str, v: Sym) {
        let mut segs = match self.pad_get(targ) {
            Sym::Hash { segs } => segs,
            other => vec![HashSeg::Rest(other)],
        };
        segs.push(HashSeg::Pairs(vec![(k.to_string(), v)]));
        self.env.top_mut().pads.insert(targ, Sym::Hash { segs });
    }

    /// `$a[i] = v` on a pad array of known length.
    fn array_store(&mut self, targ: u64, i: i64, v: Sym) {
        let cur = self.pad_get(targ);
        let new = match view(&cur) {
            (mut items, None) if i >= 0 => {
                let i = i as usize;
                if items.len() <= i {
                    items.resize(i + 1, Sym::undef());
                }
                items[i] = v;
                mk_list(items, None)
            }
            _ => Sym::Unknown("array"),
        };
        self.env.top_mut().pads.insert(targ, new);
    }

    fn assign_list(&mut self, lhs: &OpNode, rhs: Sym, source: &'static str) {
        let l = lhs.skip_null();
        let targets: Vec<&OpNode> = if l.name == "list" || (l.op_type == 0 && !l.kids.is_empty())
        {
            l.kids
                .iter()
                .filter(|k| !matches!(k.name.as_str(), "pushmark" | "padrange"))
                .filter(|k| !(k.op_type == 0 && k.kids.is_empty()))
                .collect()
        } else {
            vec![lhs]
        };
        let mut i = 0usize;
        for t in targets {
            let ts = t.skip_null();
            match ts.name.as_str() {
                "padav" => {
                    let rest = list_drop(&rhs, i);
                    self.pad_set(ts.targ, rest, source);
                    return;
                }
                "padhv" => {
                    let rest = list_drop(&rhs, i);
                    self.pad_set(ts.targ, to_hash(&rest), source);
                    return;
                }
                "rv2av" | "rv2hv" => {
                    let rest = list_drop(&rhs, i);
                    let rest = if ts.name == "rv2hv" { to_hash(&rest) } else { rest };
                    self.assign(t, rest, source);
                    return;
                }
                "undef" => i += 1,
                _ => {
                    let v = list_elem(&rhs, i as i64);
                    self.assign(t, v, source);
                    i += 1;
                }
            }
        }
    }

    // ---- control flow ----

    /// In the arm where `@_` (as it is now) is false, it is known empty.
    fn refine(&mut self, c: &Cond) {
        if let Sym::Len(l) = &c.sym {
            if !c.truth && **l == self.env.top().args {
                self.env.top_mut().args = Sym::empty();
            }
        }
    }

    /// Run both arms of a symbolic condition and join them (one arm
    /// when the condition is constant). An arm that dies or returns is
    /// dropped and the condition avoiding it recorded as a precondition.
    fn fork(
        &mut self,
        c: Sym,
        mut then: impl FnMut(&mut Self) -> Sym,
        mut els: impl FnMut(&mut Self) -> Sym,
    ) -> Sym {
        match c.truth() {
            Some(true) => return then(self),
            Some(false) => return els(self),
            None => {}
        }
        if let Some(split) = table_split(&c, self.limits.table_split) {
            return self.fork_table(split, &mut then, &mut els);
        }
        let width = self.limits.join_width;
        let ct = Cond::new(c, true);
        let cf = ct.negate();
        let base = self.env.clone();
        let line = self.line;
        let outer_conditional = self.conditional();

        self.env.conds.push(ct.clone());
        self.refine(&ct);
        let vt = then(self);
        let dt = self.dead.take();
        let mut et = std::mem::replace(&mut self.env, base);
        et.conds.pop();
        self.line = line;

        self.env.conds.push(cf.clone());
        self.refine(&cf);
        let vf = els(self);
        let df = self.dead.take();
        let mut ef = self.env.clone();
        ef.conds.pop();

        for (dead, keep) in [(dt, &cf), (df, &ct)] {
            if let (Some(d), true) = (dead, self.at_top() || keep.sym.mentions_args()) {
                self.sum.preconds.push(Precond {
                    cond: keep.clone(),
                    line,
                    enforced: d == Dead::Die,
                    conditional: outer_conditional,
                });
            }
        }
        match (dt, df) {
            (None, None) => {
                self.env = Env::join(et, ef, std::slice::from_ref(&ct), std::slice::from_ref(&cf), width);
                join_syms(&[ct], vt, &[cf], vf, width)
            }
            (None, Some(_)) => {
                self.env = et;
                vt
            }
            (Some(_), None) => {
                self.env = ef;
                vf
            }
            (Some(a), Some(b)) => {
                self.env = ef;
                self.dead = Some(if a == Dead::Return || b == Dead::Return {
                    Dead::Return
                } else {
                    Dead::Die
                });
                Sym::Unknown("dead")
            }
        }
    }

    /// `exists $table{f(KEY)}` with KEY a general element: the true arm
    /// once per key K of the table (with `f(KEY)` = K and KEY known to
    /// match K), the false arm once with KEY matching none of them.
    fn fork_table(
        &mut self,
        split: TableSplit,
        then: &mut dyn FnMut(&mut Self) -> Sym,
        els: &mut dyn FnMut(&mut Self) -> Sym,
    ) -> Sym {
        let width = self.limits.join_width;
        let base = self.env.clone();
        let line = self.line;
        let mut outs: Vec<(Vec<Cond>, Sym, Env)> = Vec::new();
        let mut dead_all: Option<Dead> = None;
        let arms: Vec<(Option<&String>, Constraint)> = split
            .keys
            .iter()
            .map(|k| {
                (
                    Some(k),
                    Constraint::Matches {
                        norm: split.norm.clone(),
                        eq: k.clone(),
                    },
                )
            })
            .chain(std::iter::once((
                None,
                Constraint::NotIn {
                    norm: split.norm.clone(),
                    keys: split.keys.clone(),
                },
            )))
            .collect();
        for (k, constraint) in arms {
            self.env = base.clone();
            self.line = line;
            let general = Sym::Elem(split.list.clone(), Key::Any(constraint));
            let key_val = match k {
                Some(k) => Sym::pv(k),
                None => substitute(&split.key, &split.elem, &general),
            };
            for v in self.env.top_mut().pads.values_mut() {
                *v = substitute(&substitute(v, &split.key, &key_val), &split.elem, &general);
            }
            let cond = Cond::new(
                builtins::apply("eq", vec![split.key.clone(), key_val.clone()]),
                k.is_some(),
            );
            self.env.conds.push(cond.clone());
            let v = if k.is_some() { then(self) } else { els(self) };
            self.env.conds.pop();
            match self.dead.take() {
                None => outs.push((vec![cond], v, self.env.clone())),
                Some(d) => dead_all = Some(d),
            }
        }
        let mut it = outs.into_iter();
        let Some((mut cs, mut val, mut env)) = it.next() else {
            self.env = base;
            self.dead = dead_all;
            return Sym::Unknown("dead");
        };
        for (c2, v2, e2) in it {
            val = join_syms(&cs, val, &c2, v2, width);
            env = Env::join(env, e2, &cs, &c2, width);
            cs = Vec::new();
        }
        self.env = env;
        val
    }

    fn eval_logop(&mut self, n: &OpNode, want: Want) -> Sym {
        let Some(lk) = n.kids.first() else {
            return Sym::Unknown("logop");
        };
        let a = self.eval(lk, Want::Scalar);
        let Some(rk) = n.kids.get(1) else {
            return a;
        };
        let keep = a.clone();
        let v = match n.name.as_str() {
            "and" => self.fork(a, |x| x.eval(rk, want), |_| keep.clone()),
            "or" => self.fork(a, |_| keep.clone(), |x| x.eval(rk, want)),
            _ => {
                let c = builtins::apply("defined", vec![a]);
                self.fork(c, |_| keep.clone(), |x| x.eval(rk, want))
            }
        };
        if n.name != "and" {
            self.note_default(&v);
        }
        v
    }

    /// Record the default of an argument-derived value.
    fn note_default(&mut self, v: &Sym) {
        if let Sym::Default { arg, default, .. } = v {
            if arg.mentions_args() {
                let (arg, d) = ((**arg).clone(), (**default).clone());
                self.note_read(&arg, ReadKind::Default(d), None, None);
            }
        }
    }

    fn eval_logassign(&mut self, n: &OpNode) -> Sym {
        let (Some(lk), Some(rk)) = (n.kids.first(), n.kids.get(1)) else {
            return Sym::Unknown("logassign");
        };
        let a = self.eval(lk, Want::Scalar);
        let keep = a.clone();
        let assign = |x: &mut Self| {
            let v = x.eval(rk, Want::Scalar);
            x.assign(lk, v.clone(), "expr");
            v
        };
        let v = match n.name.as_str() {
            "andassign" => self.fork(a, assign, |_| keep.clone()),
            "orassign" => self.fork(a, |_| keep.clone(), assign),
            _ => {
                let c = builtins::apply("defined", vec![a]);
                self.fork(c, |_| keep.clone(), assign)
            }
        };
        self.note_default(&v);
        v
    }

    fn eval_loop(&mut self, n: &OpNode) {
        let Some(head) = n.kids.first() else {
            return;
        };
        let body = n.kids.get(1).map(|b| b.skip_null());
        match head.name.as_str() {
            "enteriter" => self.eval_foreach(head, body),
            "enterloop" => {
                let Some(b) = body else { return };
                match b.name.as_str() {
                    "and" | "or" if b.kids.len() == 2 => {
                        let want_true = b.name == "and";
                        for _ in 0..self.limits.loop_iters {
                            let c = self.eval(&b.kids[0], Want::Scalar);
                            match c.truth().map(|t| t == want_true) {
                                Some(false) => break,
                                Some(true) => {
                                    self.eval(&b.kids[1], Want::Scalar);
                                    if self.dead.is_some() || self.sum.exhausted {
                                        break;
                                    }
                                }
                                None => {
                                    let base = self.env.clone();
                                    let cond = Cond::new(c, want_true);
                                    self.env.conds.push(cond);
                                    self.loop_depth += 1;
                                    self.eval(&b.kids[1], Want::Scalar);
                                    self.loop_depth -= 1;
                                    self.env.conds.pop();
                                    self.after_loop_body(base);
                                    break;
                                }
                            }
                        }
                    }
                    _ => {
                        // a bare block (or while (1)): once
                        self.eval(b, Want::Scalar);
                    }
                }
            }
            _ => {
                self.eval_kids_generic(n);
            }
        }
    }

    /// Join the state after a loop body that may run zero times.
    fn after_loop_body(&mut self, base: Env) {
        let width = self.limits.join_width;
        if self.dead.take().is_some() {
            self.env = base;
        } else {
            let after = std::mem::replace(&mut self.env, base.clone());
            self.env = Env::join(after, base, &[], &[], width);
        }
    }

    fn eval_foreach(&mut self, head: &OpNode, body: Option<&OpNode>) {
        let body = body.and_then(|b| {
            if b.name == "and" {
                b.kids.get(1)
            } else {
                Some(b)
            }
        });
        // the loop variable: a pad entry (targ) or a GV as the last kid
        let gvar = if head.targ == 0 {
            head.kids
                .last()
                .map(|k| k.skip_null())
                .and_then(gv_of)
                .map(|(nm, st)| format!("${}::{}", st, nm))
        } else {
            None
        };
        let list_kids: Vec<OpNode> = head
            .kids
            .iter()
            .filter(|k| !(k.op_type == 0 && k.kids.is_empty()))
            .filter(|k| gv_of(k.skip_null()).is_none() || k.skip_null().name != "gv")
            .cloned()
            .collect();
        let list = if head.flags & OPF_STACKED != 0 && list_kids.len() == 2 {
            // for (LO..HI)
            let lo = self.eval(&list_kids[0], Want::Scalar);
            let hi = self.eval(&list_kids[1], Want::Scalar);
            match (lo.const_int(), hi.const_int()) {
                (Some(l), Some(h)) if h - l < self.limits.loop_iters as i64 => {
                    mk_list((l..=h).map(Sym::iv).collect(), None)
                }
                _ => Sym::Unknown("range"),
            }
        } else {
            self.eval_list(&list_kids, Want::List)
        };
        let saved = gvar.as_ref().map(|g| self.env.globals.get(g).cloned());
        let set_var = |x: &mut Self, v: Sym| match &gvar {
            Some(g) => {
                x.env.globals.insert(g.clone(), v);
            }
            None => {
                x.env.top_mut().pads.insert(head.targ, v);
            }
        };
        let (items, tail) = match &list {
            Sym::Join(_) => (Vec::new(), Some(list.clone())),
            l => view(l),
        };
        if let Some(b) = body {
            for item in items.into_iter().take(self.limits.loop_iters as usize) {
                if self.dead.is_some() || self.sum.exhausted {
                    break;
                }
                set_var(self, item);
                self.eval(b, Want::Scalar);
            }
            if let Some(t) = tail {
                if self.dead.is_none() {
                    if t.as_arg_slice().is_some() {
                        self.note_read(&t, ReadKind::Iter, None, None);
                    }
                    let base = self.env.clone();
                    set_var(self, Sym::Elem(Box::new(t), Key::Any(Constraint::Unconstrained)));
                    self.loop_depth += 1;
                    self.eval(b, Want::Scalar);
                    self.loop_depth -= 1;
                    self.after_loop_body(base);
                }
            }
        }
        if let (Some(g), Some(prev)) = (&gvar, saved) {
            match prev {
                Some(v) => self.env.globals.insert(g.clone(), v),
                None => self.env.globals.remove(g),
            };
        }
    }

    // ---- calls ----

    fn call_parts<'n>(&self, n: &'n OpNode) -> (Vec<&'n OpNode>, Option<Callee<'n>>) {
        let kids: Vec<&OpNode> = match n.kids.as_slice() {
            [only] if only.op_type == 0 && !only.kids.is_empty() => only.kids.iter().collect(),
            ks => ks.iter().collect(),
        };
        let mut kids: Vec<&OpNode> = kids
            .into_iter()
            .filter(|k| !matches!(k.name.as_str(), "pushmark" | "padrange"))
            .filter(|k| !(k.op_type == 0 && k.kids.is_empty()))
            .collect();
        let Some(last) = kids.pop() else {
            return (kids, None);
        };
        let callee = match last.name.as_str() {
            "method_named" | "method" | "method_super" | "method_redir" | "method_redir_super" => {
                let name = match &last.detail {
                    OpDetail::Method { name: Some(m) } => m.clone(),
                    _ => "<dynamic>".into(),
                };
                Callee::Method {
                    name,
                    sup: last.name.ends_with("super"),
                }
            }
            _ => {
                let k = last.skip_null();
                match gv_of(k) {
                    Some((nm, st)) if k.name == "gv" => Callee::Named(format!("{}::{}", st, nm)),
                    _ => Callee::Dynamic(k),
                }
            }
        };
        (kids, Some(callee))
    }

    fn args_front(&self) -> u32 {
        let (items, tail) = view(&self.env.top().args);
        match tail.and_then(|t| t.as_arg_slice()) {
            Some((f, _)) if items.is_empty() => f,
            _ => 0,
        }
    }

    fn eval_call(&mut self, n: &OpNode, want: Want) -> Sym {
        let (arg_nodes, callee) = self.call_parts(n);
        let mut parts = Vec::new();
        let frame = (self.env.frames.len() - 1) as u32;
        for a in &arg_nodes {
            let v = self.eval(a, Want::List);
            // a variable passed to a sub is aliased by its $_[i]
            let k = a.skip_null();
            parts.push(if k.name == "padsv" && k.flags & OPF_MOD != 0 {
                Sym::List {
                    items: vec![Sym::Alias { frame, targ: k.targ }],
                    tail: None,
                }
            } else {
                v
            });
        }
        let (name, method) = match callee {
            Some(Callee::Named(s)) => (s, false),
            Some(Callee::Method { name, sup }) => {
                (if sup { format!("SUPER::{}", name) } else { name }, true)
            }
            Some(Callee::Dynamic(k)) => {
                self.eval(k, Want::Scalar);
                ("<dynamic>".to_string(), false)
            }
            None => ("<dynamic>".to_string(), false),
        };
        if self.dead.is_some() {
            return Sym::Unknown("dead");
        }
        let pkg = self.ir().package_name().to_string();
        let name = if method {
            name
        } else {
            self.src.canonical_name(&name, &pkg).unwrap_or(name)
        };
        if !method && is_die_sub(&name) {
            self.dead = Some(Dead::Die);
            return Sym::Unknown("die");
        }
        let cur_args = self.env.top().args.clone();
        let amper = !method && n.flags & OPF_STACKED == 0;
        let forward = if amper {
            Some(("amper", self.args_front()))
        } else {
            parts
                .iter()
                .find_map(|p| {
                    let (_, tail) = view(p);
                    tail.filter(|t| *t == cur_args)
                        .and_then(|t| t.as_arg_slice())
                })
                .map(|(f, _)| (if f == 0 { "all" } else { "rest" }, f))
        };
        let all = if amper {
            cur_args.clone()
        } else {
            list_concat(parts)
        };
        let (mut args, tail) = view(&all);
        if let Some(t) = tail {
            args.push(t);
        }
        if !method && name == "UNIVERSAL::isa" {
            let args = args.into_iter().map(|a| self.resolve(a)).collect();
            return builtins::apply("isa", args);
        }
        if !method && (name == "PadWalker::var_name" || name.ends_with("::var_name")) {
            // the name of the variable a reference points at
            return match args.get(1) {
                Some(Sym::RefPad { frame, targ }) => self
                    .frame_ir(*frame)
                    .pad_name(*targ)
                    .map_or_else(Sym::undef, Sym::pv),
                _ => Sym::undef(),
            };
        }

        // inline the callee when the source supplies its body
        let inline_ir = if method
            || self.env.frames.len() > self.limits.inline_depth as usize
            || self.env.frames.iter().any(|f| f.callee == name)
        {
            None
        } else {
            self.src.sub_ir(&name, &pkg)
        };
        if self.at_top() {
            if let (Some((kind, from)), None) = (forward, &inline_ir) {
                self.sum.forwards.push(ForwardEvent {
                    callee: name.clone(),
                    line: self.line,
                    kind,
                    from,
                    method,
                });
            }
            let resolved = args.iter().map(|a| self.resolve(a.clone())).collect();
            self.sum.calls.push(CallEvent {
                callee: name.clone(),
                args: resolved,
                line: self.line,
                method,
                inlined: inline_ir.is_some(),
            });
        }
        if let Some(cir) = inline_ir {
            return self.inline(cir, name, all, want);
        }
        Sym::Call {
            callee: name,
            args: args.into_iter().map(|a| self.resolve(a)).collect(),
            method,
        }
    }

    /// Execute the body of a callee in a new frame whose `@_` is `args`;
    /// the value is the join of its `return`s and its last statement.
    fn inline(&mut self, cir: Arc<SubIr>, callee: String, args: Sym, want: Want) -> Sym {
        self.inlines += 1;
        if self.at_top() {
            self.top_call_line = self.line;
        }
        let width = self.limits.join_width;
        let base_conds = self.env.conds.len();
        let line = self.line;
        self.env.frames.push(Frame {
            pads: Default::default(),
            args,
            callee,
        });
        self.ir_stack.push(cir.clone());
        self.inline_rets.push(Vec::new());
        let v = match crate::passes::body(&cir) {
            Some(b) => self.eval_block(b, Want::List),
            None => Sym::empty(),
        };
        let mut outs = self.inline_rets.pop().unwrap_or_default();
        self.ir_stack.pop();
        match self.dead.take() {
            None => outs.push((v, self.env.clone())),
            Some(Dead::Return) => {}
            Some(Dead::Die) => {
                if outs.is_empty() {
                    self.dead = Some(Dead::Die);
                }
            }
        }
        self.line = line;
        let mut it = outs.into_iter();
        let Some((mut val, mut env)) = it.next() else {
            self.env.frames.pop();
            self.env.conds.truncate(base_conds);
            return Sym::Unknown("die");
        };
        for (v2, e2) in it {
            let ca: Vec<Cond> = env.conds.get(base_conds..).unwrap_or_default().to_vec();
            let cb: Vec<Cond> = e2.conds.get(base_conds..).unwrap_or_default().to_vec();
            val = join_syms(&ca, val, &cb, v2, width);
            env = Env::join(env, e2, &ca, &cb, width);
        }
        env.frames.pop();
        env.conds.truncate(base_conds);
        self.env = env;
        match want {
            Want::List => val,
            Want::Scalar => match view(&val) {
                (items, None) => items.last().cloned().unwrap_or_else(Sym::undef),
                _ => Sym::Unknown("call"),
            },
        }
    }

    /// `@a[LIST]`; `@_[N..$#_]` is the rest of the arguments from N.
    fn eval_aslice(&mut self, n: &OpNode, want: Want) -> Sym {
        let kids: Vec<&OpNode> = n.kids.iter().filter(|k| k.name != "pushmark").collect();
        let (Some(ix), Some(ak)) = (kids.first(), kids.get(1)) else {
            return Sym::Unknown("aslice");
        };
        let t = self.array_target(ak.skip_null());
        let a = self.array_get(&t);
        let to_end = (|| {
            let flop = ix.skip_null();
            let range = flop.kids.first()?.skip_null().kids.first()?.skip_null();
            if flop.name != "flop" || range.name != "range" {
                return None;
            }
            let lo = match &range.kids.first()?.skip_null().detail {
                OpDetail::Const(lit) => Sym::Const(lit.clone()).const_int()?,
                _ => return None,
            };
            let hi = range.kids.get(1)?.skip_null();
            let hi_arr = hi.kids.first()?.skip_null();
            let same = match (&t, hi_arr.name.as_str()) {
                (ArrayTarget::Args, "rv2av") => hi_arr.kids.first().is_some_and(|k| is_args_gv(k.skip_null())),
                (ArrayTarget::Pad(targ), "padav") => hi_arr.targ == *targ,
                _ => false,
            };
            (hi.name == "av2arylen" && same && lo >= 0).then_some(lo as usize)
        })();
        let v = match to_end {
            Some(lo) => list_drop(&a, lo),
            None => {
                let idx = self.eval(ix, Want::List);
                match view(&idx) {
                    (items, None) => {
                        let mut out = Vec::new();
                        for i in &items {
                            match i.const_int() {
                                Some(i) => out.push(list_elem(&a, i)),
                                None => return Sym::Unknown("aslice"),
                            }
                        }
                        mk_list(out, None)
                    }
                    _ => Sym::Unknown("aslice"),
                }
            }
        };
        match want {
            Want::List => v,
            Want::Scalar => match view(&v) {
                (items, None) => items.last().cloned().unwrap_or_else(Sym::undef),
                _ => Sym::Unknown("aslice"),
            },
        }
    }

    fn eval_goto(&mut self, n: &OpNode) -> Sym {
        fn find_gv(n: &OpNode) -> Option<String> {
            if n.name == "gv" {
                if let Some((nm, st)) = gv_of(n) {
                    return Some(format!("{}::{}", st, nm));
                }
            }
            n.kids.iter().find_map(find_gv)
        }
        let Some(k) = n.kids.first() else {
            self.unknown("goto");
            return Sym::Unknown("goto");
        };
        let Some(name) = find_gv(k) else {
            // goto &$code
            self.eval(k, Want::Scalar);
            self.leave_with(Sym::Unknown("goto"));
            return Sym::Unknown("goto");
        };
        if is_die_sub(&name) {
            self.dead = Some(Dead::Die);
            return Sym::Unknown("die");
        }
        let from = self.args_front();
        let args = vec![self.env.top().args.clone()];
        if self.at_top() {
            self.sum.forwards.push(ForwardEvent {
                callee: name.clone(),
                line: self.line,
                kind: "goto",
                from,
                method: false,
            });
            self.sum.calls.push(CallEvent {
                callee: name.clone(),
                args: args.clone(),
                line: self.line,
                method: false,
                inlined: false,
            });
        }
        let v = Sym::Call {
            callee: name,
            args,
            method: false,
        };
        self.leave_with(v.clone());
        v
    }

    /// Leave the current sub with value `v` (`return`, `goto &f`).
    fn leave_with(&mut self, v: Sym) {
        match self.inline_rets.last_mut() {
            Some(rets) => rets.push((v, self.env.clone())),
            None => {
                self.sum.returns.push(v);
                self.ret_envs.push(self.env.clone());
            }
        }
        self.dead = Some(Dead::Return);
    }
}

/// How an assigned value was taken from the arguments, by the op that
/// computes it.
fn source_of(rhs: &OpNode) -> &'static str {
    fn find(n: &OpNode, f: &dyn Fn(&OpNode) -> Option<&'static str>) -> Option<&'static str> {
        f(n).or_else(|| n.kids.iter().find_map(|k| find(k, f)))
    }
    // an op that removes arguments wins over a test of `@_` next to it
    // (`@_ ? shift : d`)
    find(rhs, &|n| match n.name.as_str() {
        "shift" => Some("shift"),
        "pop" => Some("pop"),
        "splice" => Some("splice"),
        _ => None,
    })
    .or_else(|| {
        find(rhs, &|n| match n.name.as_str() {
            "aelemfast" | "aelem" | "multideref" => Some("elem"),
            "rv2av" if n.kids.first().is_some_and(|k| is_args_gv(k.skip_null())) => {
                Some("unpack")
            }
            _ => None,
        })
    })
    .unwrap_or("expr")
}

/// The pad variable a hash/array expression names (`%args`), for
/// [`Read::via`].
fn pad_via(ir: &SubIr, n: &OpNode) -> Option<String> {
    let k = n.skip_null();
    match k.name.as_str() {
        "padhv" | "padav" | "padsv" => ir.pad_name(k.targ).map(String::from),
        "rv2hv" | "rv2av" => k.kids.first().and_then(|x| pad_via(ir, x)),
        _ => None,
    }
}

/// A hash key (or array index) from a value: a constant, a general
/// element's constraint, or the symbolic value itself.
fn key_of(v: Sym, array: bool) -> Key {
    if array {
        if let Some(i) = v.const_int() {
            return Key::Index(i);
        }
    } else if let Some(s) = v.const_str() {
        return Key::Str(s);
    }
    match v {
        Sym::Elem(_, Key::Any(c)) => Key::Any(c),
        v => Key::Sym(Box::new(v)),
    }
}

/// The literal after `^` of an anchored pattern without metacharacters
/// (`^\-` → `-`).
fn literal_prefix(p: &str) -> Option<String> {
    let rest = p.strip_prefix('^')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let e = chars.next()?;
                if e.is_ascii_alphanumeric() {
                    return None; // \d, \w, ...
                }
                out.push(e);
            }
            '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '$' | '^' => {
                return None;
            }
            c => out.push(c),
        }
    }
    (!out.is_empty()).then_some(out)
}

/// What a table split needs (see `Exec::fork_table`).
struct TableSplit {
    /// The concrete keys of the table.
    keys: Vec<String>,
    /// The looked-up key `f(KEY)`.
    key: Sym,
    /// The general element `KEY` and the list it is an element of.
    elem: Sym,
    list: Box<Sym>,
    norm: Vec<Norm>,
}

/// `exists $table{f(KEY)}` where the table's keys are all known and KEY is
/// a general element (the loop variable over a list of unknown length) put
/// through normalizations `f` (`lc`, `strip_prefix`).
fn table_split(c: &Sym, limit: u32) -> Option<TableSplit> {
    let Sym::App("exists", v) = c else {
        return None;
    };
    let [Sym::Elem(table, Key::Sym(key))] = v.as_slice() else {
        return None;
    };
    let Sym::Hash { segs } = table.as_ref() else {
        return None;
    };
    let mut keys: Vec<String> = Vec::new();
    for seg in segs {
        let HashSeg::Pairs(p) = seg else {
            return None;
        };
        for (k, _) in p {
            if !keys.contains(k) {
                keys.push(k.clone());
            }
        }
    }
    if keys.is_empty() || keys.len() > limit as usize {
        return None;
    }
    let mut norm = Vec::new();
    let mut cur: &Sym = key;
    loop {
        match cur {
            Sym::App("lc", a) if a.len() == 1 => {
                norm.push(Norm::Lc);
                cur = &a[0];
            }
            Sym::App("strip_prefix", a) if a.len() == 2 => {
                norm.push(Norm::StripPrefix(a[1].const_str()?));
                cur = &a[0];
            }
            _ => break,
        }
    }
    norm.reverse();
    let Sym::Elem(list, Key::Any(Constraint::Unconstrained)) = cur else {
        return None;
    };
    Some(TableSplit {
        keys,
        key: (**key).clone(),
        elem: cur.clone(),
        list: list.clone(),
        norm,
    })
}

/// Replace every occurrence of `from` in `s` by `to`.
fn substitute(s: &Sym, from: &Sym, to: &Sym) -> Sym {
    if s == from {
        return to.clone();
    }
    let sub = |x: &Sym| substitute(x, from, to);
    let subb = |x: &Box<Sym>| Box::new(substitute(x, from, to));
    match s {
        Sym::List { items, tail } => Sym::List {
            items: items.iter().map(sub).collect(),
            tail: tail.as_ref().map(subb),
        },
        Sym::Hash { segs } => Sym::Hash {
            segs: segs
                .iter()
                .map(|g| match g {
                    HashSeg::Pairs(p) => {
                        HashSeg::Pairs(p.iter().map(|(k, v)| (k.clone(), sub(v))).collect())
                    }
                    HashSeg::Rest(r) => HashSeg::Rest(sub(r)),
                })
                .collect(),
        },
        Sym::Ref(x) => Sym::Ref(subb(x)),
        Sym::Len(x) => Sym::Len(subb(x)),
        Sym::Deref(x, c) => Sym::Deref(subb(x), *c),
        Sym::Keys(x) => Sym::Keys(subb(x)),
        Sym::Elem(x, k) => Sym::Elem(
            subb(x),
            match k {
                Key::Sym(ks) => key_of(sub(ks), false),
                k => k.clone(),
            },
        ),
        Sym::App(op, a) => Sym::App(op, a.iter().map(sub).collect()),
        Sym::Default { arg, default, when } => Sym::Default {
            arg: subb(arg),
            default: subb(default),
            when: *when,
        },
        Sym::Call {
            callee,
            args,
            method,
        } => Sym::Call {
            callee: callee.clone(),
            args: args.iter().map(sub).collect(),
            method: *method,
        },
        Sym::Join(alts) => Sym::Join(alts.iter().map(|(c, x)| (c.clone(), sub(x))).collect()),
        other => other.clone(),
    }
}

/// The op an lvalue / reference names: through nulls and the nulled
/// element ops (`ex-aelem` over an `aelemfast`).
fn lvalue_core(n: &OpNode) -> &OpNode {
    let mut cur = n.skip_null();
    while cur.op_type == 0
        && matches!(cur.was.as_deref(), Some("aelem" | "helem" | "list"))
        && !cur.kids.is_empty()
    {
        cur = cur.kids[0].skip_null();
    }
    cur
}
