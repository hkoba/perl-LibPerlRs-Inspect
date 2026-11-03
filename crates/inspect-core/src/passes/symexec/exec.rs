//! The evaluator: walks the OpNode tree (not `op_next`) of a sub body,
//! forking at symbolic conditions and joining after each compound
//! statement.

use crate::ir::{OpDetail, OpNode, SubIr, SvLit, padname_flags};
use crate::render::render;

use super::builtins;
use super::env::{
    Env, hash_get, join_syms, list_concat, list_drop, list_elem, list_len,
    list_pop, list_shift, mk_list, to_hash, view,
};
use super::summary::{Assign, CallEvent, ForwardEvent, Precond, Read, ReadKind, Summary};
use super::value::{Cond, Constraint, HashSeg, Key, Sym};
use super::{IrSource, Limits};

const OPF_WANT: u8 = 3;
const OPF_WANT_LIST: u8 = 3;
const OPF_STACKED: u8 = 64;
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
    #[allow(dead_code)]
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

    fn next_seq(&mut self) -> u32 {
        self.seq += 1;
        self.seq
    }

    fn conditional(&self) -> bool {
        !self.env.conds.is_empty() || self.loop_depth > 0
    }

    fn note_read(&mut self, v: &Sym, kind: ReadKind, via: Option<String>, text: Option<String>) {
        if !v.mentions_args() {
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
        self.ir.pad.iter().find(|e| e.ix as u64 == targ)
    }

    fn pad_name(&self, targ: u64) -> Option<String> {
        self.ir.pad_name(targ).map(String::from)
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
        self.env
            .top()
            .pads
            .get(&targ)
            .cloned()
            .unwrap_or_else(|| self.pad_default(targ))
    }

    fn pad_set(&mut self, targ: u64, v: Sym, source: &'static str) {
        self.env.top_mut().pads.insert(targ, v.clone());
        if v.mentions_args() && !self.sum.assigns.iter().any(|a| a.targ == targ) {
            let seq = self.next_seq();
            self.sum.assigns.push(Assign {
                seq,
                targ,
                sym: v,
                line: self.line,
                conditional: self.conditional(),
                source,
            });
        }
    }

    /// Pad entry by name (multideref keys carry names, not targs);
    /// ambiguous names give None.
    fn pad_by_name(&self, name: &str) -> Option<u64> {
        let mut it = self
            .ir
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
                let v = match n.kids.first() {
                    Some(k) => self.eval(k, Want::Scalar),
                    None => Sym::Unknown("padsv_store"),
                };
                let src = n.kids.first().map_or("expr", source_of);
                self.pad_set(n.targ, v.clone(), src);
                v
            }
            "sassign" => {
                let v = match n.kids.first() {
                    Some(k) => self.eval(k, Want::Scalar),
                    None => Sym::Unknown("sassign"),
                };
                if let (Some(lhs), Some(rhs)) = (n.kids.get(1), n.kids.first()) {
                    self.assign(lhs, v.clone(), source_of(rhs));
                }
                v
            }
            "aassign" => {
                let rhs = match n.kids.first() {
                    Some(k) => self.eval(k, Want::List),
                    None => Sym::empty(),
                };
                if let Some(lhs) = n.kids.get(1) {
                    let src = if rhs.mentions_args() { "unpack" } else { "expr" };
                    self.assign_list(lhs, rhs.clone(), src);
                }
                match want {
                    Want::List => rhs,
                    Want::Scalar => list_len(&rhs),
                }
            }
            "rv2av" => {
                let t = self.array_target(n);
                let v = self.array_get(&t);
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
                v
            }
            "splice" => self.eval_splice(n, want),
            "aelemfast" => {
                let ix = n.private as i8 as i64;
                if is_args_gv(n) {
                    let v = list_elem(&self.env.top().args.clone(), ix);
                    let text = render(self.ir, n);
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
                    Some(i) => list_elem(&a, i),
                    None => Sym::Elem(Box::new(a), Key::Sym(Box::new(ix))),
                };
                if matches!(t, ArrayTarget::Args) {
                    let text = render(self.ir, n);
                    self.note_read(&v, ReadKind::Elem, None, Some(text));
                }
                v
            }
            "helem" => {
                let (Some(hk), Some(kk)) = (n.kids.first(), n.kids.get(1)) else {
                    return Sym::Unknown("helem");
                };
                let h = self.eval(hk, Want::List);
                let via = pad_via(self.ir, hk);
                let k = self.eval(kk, Want::Scalar);
                let key = match k.const_str() {
                    Some(s) => Key::Str(s),
                    None => Key::Sym(Box::new(k)),
                };
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
            "entersub" => self.eval_call(n),
            "goto" => self.eval_goto(n),
            "die" => {
                self.eval_kids_generic(n);
                self.dead = Some(Dead::Die);
                Sym::Unknown("die")
            }
            "return" => {
                let v = self.eval_list(&n.kids, Want::List);
                if self.dead.is_none() {
                    self.sum.returns.push(v.clone());
                    self.ret_envs.push(self.env.clone());
                    self.dead = Some(Dead::Return);
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
                let k = n.kids.first().map(|k| k.skip_null());
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
            Sym::Ref(x) => *x,
            Sym::RefPad { targ, .. } => self.pad_get(targ),
            Sym::Unknown(_) => r,
            other => Sym::Deref(Box::new(other), sigil),
        }
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
        let text = Some(render(self.ir, n));
        for (arg, d) in defaults {
            self.note_read(&arg, ReadKind::Default(d), via.clone(), text.clone());
        }
        self.note_read(&v, ReadKind::Key, via, text);
        v
    }

    fn mderef_key(&self, key: Option<&str>, array: bool) -> Key {
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
                let v = self.pad_get(targ);
                return match (array, v.const_int(), v.const_str()) {
                    (true, Some(i), _) => Key::Index(i),
                    (false, _, Some(s)) => Key::Str(s),
                    _ => Key::Sym(Box::new(v)),
                };
            }
            if !array {
                return Key::Sym(Box::new(Sym::Unknown("key")));
            }
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
        let text = render(self.ir, n);
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
            let key = self.mderef_key(st.key.as_deref(), array);
            let v = if array {
                match &key {
                    Key::Index(i) => list_elem(&container, *i),
                    k => Sym::Elem(Box::new(container), k.clone()),
                }
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
        let l = lhs.skip_null();
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
                        ("padhv", _, Some(k)) if !k.starts_with('$') => {
                            let targ = st.base_targ.unwrap_or(0);
                            let h = self.pad_get(targ);
                            let mut segs = match h {
                                Sym::Hash { segs } => segs,
                                other => vec![HashSeg::Rest(other)],
                            };
                            segs.push(HashSeg::Pairs(vec![(k.to_string(), v)]));
                            self.env.top_mut().pads.insert(targ, Sym::Hash { segs });
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
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
        then: impl FnOnce(&mut Self) -> Sym,
        els: impl FnOnce(&mut Self) -> Sym,
    ) -> Sym {
        match c.truth() {
            Some(true) => return then(self),
            Some(false) => return els(self),
            None => {}
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
            if let Some(d) = dead {
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
            "and" => self.fork(a, |x| x.eval(rk, want), |_| keep),
            "or" => self.fork(a, |_| keep, |x| x.eval(rk, want)),
            _ => {
                let c = builtins::apply("defined", vec![a]);
                self.fork(c, |_| keep, |x| x.eval(rk, want))
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
            "andassign" => self.fork(a, assign, |_| keep),
            "orassign" => self.fork(a, |_| keep, assign),
            _ => {
                let c = builtins::apply("defined", vec![a]);
                self.fork(c, |_| keep, assign)
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
                        self.loop_depth += 1;
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
                                    self.eval(&b.kids[1], Want::Scalar);
                                    self.env.conds.pop();
                                    self.after_loop_body(base);
                                    break;
                                }
                            }
                        }
                        self.loop_depth -= 1;
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
        self.loop_depth += 1;
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
                    self.eval(b, Want::Scalar);
                    self.after_loop_body(base);
                }
            }
        }
        self.loop_depth -= 1;
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

    fn eval_call(&mut self, n: &OpNode) -> Sym {
        let (arg_nodes, callee) = self.call_parts(n);
        let mut parts = Vec::new();
        for a in &arg_nodes {
            parts.push(self.eval(a, Want::List));
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
        if let Some((kind, from)) = forward {
            self.sum.forwards.push(ForwardEvent {
                callee: name.clone(),
                line: self.line,
                kind,
                from,
                method,
            });
        }
        let all = list_concat(parts);
        let (mut args, tail) = view(&all);
        if amper {
            args = vec![cur_args];
        } else if let Some(t) = tail {
            args.push(t);
        }
        self.sum.calls.push(CallEvent {
            callee: name.clone(),
            args: args.clone(),
            line: self.line,
            method,
            inlined: false,
        });
        Sym::Call {
            callee: name,
            args,
            method,
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
            self.ret_envs.push(self.env.clone());
            self.sum.returns.push(Sym::Unknown("goto"));
            self.dead = Some(Dead::Return);
            return Sym::Unknown("goto");
        };
        if is_die_sub(&name) {
            self.dead = Some(Dead::Die);
            return Sym::Unknown("die");
        }
        let from = self.args_front();
        let args = vec![self.env.top().args.clone()];
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
        let v = Sym::Call {
            callee: name,
            args,
            method: false,
        };
        self.sum.returns.push(v.clone());
        self.ret_envs.push(self.env.clone());
        self.dead = Some(Dead::Return);
        v
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
