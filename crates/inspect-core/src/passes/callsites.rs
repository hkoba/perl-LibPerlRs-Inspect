//! Call sites of a sub: every `entersub` (and `goto &sub`) with its callee,
//! its arguments as written and what is known about their count.
//!
//! [`call_sites`] lists the calls in pre-order of the op tree (`node` ids
//! increase in that order). Perl keeps line numbers per statement, so
//! `line` is the line of the statement a call belongs to (the first line
//! of a statement written over several lines). To tell apart calls on one
//! line, [`CallSite::line_seq`] numbers them in source order of their
//! callee names, which pre-order does not always follow: a method chain
//! `$x->a->b` is `b(a($x))` in the tree, and an assignment evaluates its
//! right-hand side first.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::ir::{OpDetail, OpNode, SubIr, SvLit};
use crate::render::render;

/// One call in the body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CallSite {
    /// Node id of the `entersub` / `goto`.
    pub node: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
    pub callee: Callee,
    /// The arguments (the invocant of a method call excluded).
    pub args: Vec<ArgExpr>,
    /// Number of arguments passed (invocant excluded).
    pub argc: Arity,
    /// The constant-keyed `key => value` pairs at the end of the
    /// arguments (values as source text, `None` when not a plain scalar).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pairs: Option<Vec<(String, Option<String>)>>,
    /// Index into `args` where `pairs` start.
    #[serde(default)]
    pub pairs_from: u64,
    /// The call passes the caller's `@_` on (`f(@_)`, `&f;`, `goto &f`).
    #[serde(deserialize_with = "super::bool_or_int")]
    pub forwards_args: bool,
    /// call | amper | goto | method
    pub kind: String,
    /// Position of the call among the calls with the same `line`, in the
    /// source order of their callee names (0-based). Best effort, from
    /// the shape of the op tree: a method's invocant precedes it and the
    /// left-hand side of an assignment precedes the right-hand side;
    /// otherwise operands are in tree order, so `f() if g()` (the same
    /// tree as `g() && f()`) numbers `g` first.
    #[serde(default)]
    pub line_seq: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Callee {
    /// A named sub, package-qualified (`Fixtures::foo`).
    Named { name: String },
    /// `$inv->name(...)` / `Class->name(...)`.
    Method { name: String, invocant: Invocant },
    /// `$self->SUPER::name(...)`.
    Super { name: String },
    /// A code reference or a dynamic method name, as source text.
    Dynamic(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Invocant {
    /// A class name constant (`Klass->new`).
    Class(String),
    /// A lexical variable (`$self->m`, `$o->m`).
    SelfVar(String),
    Expr(String),
}

/// An argument expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgExpr {
    pub text: String,
    /// scalar | list | hash | args | call — all but scalar have a length
    /// not known at the call site.
    pub shape: String,
}

/// Bounds on a number of arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arity {
    pub min: u64,
    /// None = unbounded.
    pub max: Option<u64>,
}

const OPF_STACKED: u8 = 64;

/// All call sites of `ir`, in pre-order of the op tree.
pub fn call_sites(ir: &SubIr) -> Vec<CallSite> {
    let mut out = Vec::new();
    super::walk_with_lines(ir, |n, line| match n.name.as_str() {
        "entersub" => out.push(entersub_site(ir, n, line)),
        "goto" => {
            if let Some(site) = goto_site(ir, n, line) {
                out.push(site);
            }
        }
        _ => {}
    });
    number_in_lines(ir, &mut out);
    out
}

/// Set [`CallSite::line_seq`].
fn number_in_lines(ir: &SubIr, sites: &mut [CallSite]) {
    let mut order: Vec<u32> = Vec::new();
    source_order(&ir.root, &mut order);
    let rank: HashMap<u32, usize> = order.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut ix: Vec<usize> = (0..sites.len()).collect();
    ix.sort_by_key(|&i| (sites[i].line, rank.get(&sites[i].node).copied().unwrap_or(usize::MAX), i));
    let mut prev: Option<Option<u32>> = None;
    let mut seq = 0;
    for i in ix {
        if prev != Some(sites[i].line) {
            prev = Some(sites[i].line);
            seq = 0;
        }
        sites[i].line_seq = seq;
        seq += 1;
    }
}

/// Node ids of the calls (`entersub` / `goto`) under `n`, in the order
/// their callee names appear in the source (see [`CallSite::line_seq`]).
fn source_order(n: &OpNode, out: &mut Vec<u32>) {
    match n.name.as_str() {
        "entersub" => {
            let (args, last) = split_entersub(n);
            match last {
                Some(m) if m.name.starts_with("method") => {
                    let mut rest = args.as_slice();
                    if let Some((inv, more)) = rest.split_first() {
                        source_order(inv, out);
                        rest = more;
                    }
                    out.push(n.id);
                    rest.iter().for_each(|a| source_order(a, out));
                    source_order(m, out);
                }
                _ => {
                    out.push(n.id);
                    if let Some(c) = last {
                        source_order(c, out);
                    }
                    args.iter().for_each(|a| source_order(a, out));
                }
            }
        }
        "goto" => {
            out.push(n.id);
            n.kids.iter().for_each(|k| source_order(k, out));
        }
        // the right-hand side is the first kid
        "sassign" | "aassign" if n.kids.len() == 2 => {
            source_order(&n.kids[1], out);
            source_order(&n.kids[0], out);
        }
        _ => n.kids.iter().for_each(|k| source_order(k, out)),
    }
}

fn gv_name(n: &OpNode) -> Option<String> {
    match &n.detail {
        OpDetail::Gv { name, stash } => Some(format!(
            "{}::{}",
            stash.as_deref().unwrap_or("main"),
            name
        )),
        _ => None,
    }
}

fn is_args_av(n: &OpNode) -> bool {
    n.name == "rv2av"
        && n.kids.first().is_some_and(|k| {
            matches!(&k.skip_null().detail, OpDetail::Gv { name, stash }
                if name == "_" && stash.as_deref().unwrap_or("main") == "main")
        })
}

fn shape_of(n: &OpNode) -> &'static str {
    let k = n.skip_null();
    match k.name.as_str() {
        _ if is_args_av(k) => "args",
        "padav" | "rv2av" | "aslice" | "kvaslice" | "hslice" | "kvhslice" | "sort" | "grepwhile"
        | "mapwhile" | "keys" | "values" | "split" | "reverse" | "flop" | "range" | "splice" => {
            "list"
        }
        "padhv" | "rv2hv" => "hash",
        "entersub" => "call",
        _ => "scalar",
    }
}

fn const_str(n: &OpNode) -> Option<String> {
    match &n.skip_null().detail {
        OpDetail::Const(SvLit::Pv(s)) => Some(s.clone()),
        _ => None,
    }
}

/// The kids of an entersub without the pushmark: `(args, callee op)`.
fn split_entersub(n: &OpNode) -> (Vec<&OpNode>, Option<&OpNode>) {
    let kids: Vec<&OpNode> = match n.kids.as_slice() {
        [only] if only.op_type == 0 && !only.kids.is_empty() => only.kids.iter().collect(),
        ks => ks.iter().collect(),
    };
    let mut kids: Vec<&OpNode> = kids
        .into_iter()
        .filter(|k| !matches!(k.name.as_str(), "pushmark" | "padrange"))
        .filter(|k| !(k.op_type == 0 && k.kids.is_empty()))
        .collect();
    let last = kids.pop();
    (kids, last)
}

fn entersub_site(ir: &SubIr, n: &OpNode, line: Option<u32>) -> CallSite {
    let (mut arg_nodes, last) = split_entersub(n);
    let mut kind = "call";
    let callee = match last {
        Some(m) if m.name.starts_with("method") => {
            kind = "method";
            let name = match &m.detail {
                OpDetail::Method { name: Some(s) } => Some(s.clone()),
                _ => None,
            };
            let inv = if arg_nodes.is_empty() {
                None
            } else {
                Some(arg_nodes.remove(0))
            };
            match (name, m.name.contains("super")) {
                (Some(name), true) => Callee::Super { name },
                (Some(name), false) => Callee::Method {
                    name,
                    invocant: invocant(ir, inv),
                },
                (None, _) => Callee::Dynamic(
                    m.kids.first().map_or_else(|| "<method>".into(), |k| render(ir, k)),
                ),
            }
        }
        Some(c) => {
            let k = c.skip_null();
            match gv_name(k) {
                Some(name) if k.name == "gv" => Callee::Named { name },
                _ => Callee::Dynamic(render(ir, k)),
            }
        }
        None => Callee::Dynamic("<none>".into()),
    };
    let amper = kind == "call" && n.flags & OPF_STACKED == 0;
    if amper {
        kind = "amper";
    }
    let args: Vec<ArgExpr> = arg_nodes
        .iter()
        .map(|a| ArgExpr {
            text: render(ir, a),
            shape: shape_of(a).into(),
        })
        .collect();
    let (pairs_from, pairs) = pairs_of(ir, &arg_nodes);
    CallSite {
        node: n.id,
        line,
        callee,
        argc: arity_of(&args, amper),
        forwards_args: amper || args.iter().any(|a| a.shape == "args"),
        args,
        pairs,
        pairs_from,
        kind: kind.into(),
        line_seq: 0,
    }
}

fn invocant(ir: &SubIr, n: Option<&OpNode>) -> Invocant {
    let Some(n) = n else {
        return Invocant::Expr("<none>".into());
    };
    if let Some(s) = const_str(n) {
        return Invocant::Class(s);
    }
    let k = n.skip_null();
    if k.name == "padsv" {
        if let Some(name) = ir.pad_name(k.targ) {
            return Invocant::SelfVar(name.into());
        }
    }
    Invocant::Expr(render(ir, n))
}

fn arity_of(args: &[ArgExpr], amper: bool) -> Arity {
    if amper {
        return Arity { min: 0, max: None };
    }
    let scalars = args.iter().filter(|a| a.shape == "scalar").count() as u64;
    let open = args.iter().any(|a| a.shape != "scalar");
    Arity {
        min: scalars,
        max: (!open).then_some(scalars),
    }
}

/// The shortest prefix after which the scalar arguments are all
/// `'key' => value` pairs (at least one pair).
fn pairs_of(ir: &SubIr, args: &[&OpNode]) -> (u64, Option<Vec<(String, Option<String>)>>) {
    let n = args.len();
    for from in 0..n {
        let rest = &args[from..];
        if rest.len() < 2 || !rest.len().is_multiple_of(2) {
            continue;
        }
        if rest.iter().any(|a| shape_of(a) != "scalar") {
            continue;
        }
        let pairs: Option<Vec<(String, Option<String>)>> = rest
            .chunks(2)
            .map(|kv| const_str(kv[0]).map(|k| (k, Some(render(ir, kv[1])))))
            .collect();
        if let Some(p) = pairs {
            return (from as u64, Some(p));
        }
    }
    (0, None)
}

/// `goto &name`: a call that passes the current `@_`.
fn goto_site(ir: &SubIr, n: &OpNode, line: Option<u32>) -> Option<CallSite> {
    fn find_gv(n: &OpNode) -> Option<String> {
        if n.name == "gv" {
            return gv_name(n);
        }
        n.kids.iter().find_map(find_gv)
    }
    let k = n.kids.first()?;
    let callee = match find_gv(k) {
        Some(name) => Callee::Named { name },
        None => Callee::Dynamic(render(ir, k)),
    };
    Some(CallSite {
        node: n.id,
        line,
        callee,
        args: Vec::new(),
        argc: Arity { min: 0, max: None },
        pairs: None,
        pairs_from: 0,
        forwards_args: true,
        kind: "goto".into(),
        line_seq: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passes::test_util::ir_of;

    #[test]
    fn calls_mixed() {
        let ir = ir_of(include_str!("../../../../t/golden/5.42-threaded/calls_mixed.json"));
        let c = call_sites(&ir);
        assert_eq!(c.len(), 7);

        assert_eq!(c[0].callee, Callee::Named { name: "Fixtures::foo".into() });
        assert_eq!(c[0].argc, Arity { min: 2, max: Some(2) });
        assert_eq!(c[0].kind, "call");

        assert_eq!(c[1].callee, Callee::Named { name: "Bar::baz".into() });
        assert_eq!(c[1].argc, Arity { min: 1, max: None });
        assert_eq!(c[1].args[1].shape, "list");

        assert_eq!(c[2].kind, "method");
        assert_eq!(
            c[2].callee,
            Callee::Method {
                name: "meth".into(),
                invocant: Invocant::SelfVar("$o".into())
            }
        );
        let keys: Vec<&str> = c[2].pairs.as_ref().unwrap().iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["a", "b"]);
        assert_eq!(c[2].argc, Arity { min: 4, max: Some(4) });

        assert_eq!(
            c[3].callee,
            Callee::Method {
                name: "new".into(),
                invocant: Invocant::Class("Klass".into())
            }
        );
        assert_eq!(c[3].argc, Arity { min: 0, max: None });

        assert_eq!(c[4].kind, "amper");
        assert!(c[4].forwards_args);

        assert!(c[5].forwards_args);
        assert_eq!(c[5].args[0].shape, "args");

        assert_eq!(c[6].callee, Callee::Dynamic("$cb".into()));
        assert_eq!(c[6].argc, Arity { min: 1, max: Some(1) });
    }

    #[test]
    fn line_seq_numbers_each_line() {
        let ir = ir_of(include_str!("../../../../t/golden/5.42-threaded/calls_mixed.json"));
        let c = call_sites(&ir);
        let mut seen: Vec<(Option<u32>, u32)> = c.iter().map(|s| (s.line, s.line_seq)).collect();
        let n = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), n, "(line, line_seq) is unique: {seen:?}");
        for s in &c {
            let before = c.iter().filter(|o| o.line == s.line && o.line_seq < s.line_seq).count();
            assert_eq!(before as u32, s.line_seq, "line_seq counts from 0 per line");
        }
    }

    #[test]
    fn super_and_goto() {
        let ir = ir_of(include_str!("../../../../t/golden/5.42-threaded/args_forward.json"));
        let c = call_sites(&ir);
        assert_eq!(c[0].callee, Callee::Super { name: "new".into() });
        assert!(c[0].forwards_args);
        assert_eq!(c[3].kind, "goto");
        assert_eq!(c[3].callee, Callee::Named { name: "Fixtures::other3".into() });
    }
}
