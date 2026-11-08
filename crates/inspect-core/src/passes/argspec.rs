//! Argument spec inference.
//!
//! A signature (argcheck/argelem, or multiparam on perl 5.44+) yields the
//! exact spec directly. Everything else is a summary of the symbolic
//! execution of the whole body ([`super::symexec`]): `my (...) = @_`,
//! `shift` / `pop` / `splice`, `$_[n]`, `foreach (@_)`, hash arguments
//! (`my %args = @_; $args{key}`), defaults (`//`, `||`, `//=`, `unless
//! defined`, `@_ ? shift : d`), arity checks that die, and calls that pass
//! the remaining arguments on.

use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use crate::ir::{NamedParam, OpDetail, SubIr};
use crate::render::render;

use super::symexec::{
    self, Cond, IrSource, Key, Limits, NoSource, ReadKind, Summary, Sym, arity_offset,
};
pub use super::symexec::Norm;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgSpec {
    /// signature | unpack | shift | positional | helper | mixed | none
    pub style: String,
    pub min_arity: u64,
    /// None = no upper bound (slurpy or unchecked)
    pub max_arity: Option<u64>,
    pub params: Vec<Param>,
    pub invocant_guess: Option<String>,
    /// Keys read from hash arguments (`$args{key}`, `$opts->{key}`).
    #[serde(default)]
    pub named: Vec<NamedKey>,
    /// Calls that pass (the rest of) the arguments on.
    #[serde(default)]
    pub forwards: Vec<Forward>,
    /// Calls to argument-processing helpers (`rearrange`, `m4i_args`).
    #[serde(default)]
    pub helpers: Vec<HelperUse>,
    /// Conditions on the number of arguments.
    #[serde(default)]
    pub checks: Vec<ArityCheck>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: Option<String>,
    pub index: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub default: Option<String>,
    /// signature | unpack | shift | elem
    pub source: String,
    /// Key of a named signature parameter (`:$key`, perl 5.44+). Named
    /// parameters come after the positional ones and carry index =
    /// the number of positional parameters.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub key: Option<String>,
    /// Every argument slot the variable may hold (more than one when it
    /// depends on a branch).
    #[serde(default)]
    pub alts: Vec<Slot>,
    /// Line of the statement that takes the argument.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
    /// The argument is taken only on some paths.
    #[serde(default, deserialize_with = "super::bool_or_int")]
    pub conditional: bool,
    /// Declared class (`my Foo $x`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub typ: Option<String>,
}

/// Which part of `@_` a parameter holds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    /// `$_[n]` of the original `@_`
    Front(u64),
    /// the n-th argument from the end (0 = last)
    Back(u64),
    /// the arguments from index n on
    Rest(u64),
    /// the value of a hash argument key
    Key(String),
    /// the number of arguments
    Count,
    Unknown,
}

/// A key read from a hash of arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedKey {
    pub key: String,
    /// The variable the key is read through (`%args`, `$opts`) or the
    /// helper that defines it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub via: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub default: Option<String>,
    /// Read on every path without a default or an `exists` test.
    #[serde(default, deserialize_with = "super::bool_or_int")]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
    /// Normalizations applied to the caller's key before the match.
    #[serde(default)]
    pub normalize: Vec<Norm>,
    /// Other keys that map to the same parameter.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Declared type (`Smart::Args` rules: `my $x => 'Int'`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub typ: Option<String>,
}

/// A call that passes the arguments on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Forward {
    pub callee: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
    /// rest (`f(@_)` after shifts) | all (`f(@_)`) | goto (`goto &f`) |
    /// amper (`&f;`)
    pub kind: String,
    /// Index of the first original argument passed on.
    pub from: u32,
}

/// A call to an argument-processing helper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelperUse {
    pub callee: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
    #[serde(deserialize_with = "super::bool_or_int")]
    pub inlined: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub note: Option<String>,
}

/// A condition on the number of arguments, as the condition the
/// arguments must satisfy (`croak unless @_ == 2` → `== 2`, `die if @_ >
/// 5` → `<= 5`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArityCheck {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
    /// == | != | >= | <= | > | <
    pub cmp: String,
    pub n: Option<u64>,
    /// The other path dies (otherwise it returns early).
    #[serde(deserialize_with = "super::bool_or_int")]
    pub enforced: bool,
}

pub fn analyze_args(ir: &SubIr) -> ArgSpec {
    analyze_args_with(ir, &NoSource, &Limits::default())
}

/// [`analyze_args`] with helper bodies supplied by `src` (inlined into the
/// symbolic execution).
pub fn analyze_args_with(ir: &SubIr, src: &dyn IrSource, limits: &Limits) -> ArgSpec {
    if let Some(spec) = detect_signature(ir) {
        return spec;
    }
    let sum = symexec::exec_sub(ir, src, limits);
    spec_from_summary(ir, &sum)
}

/// signature: argcheck / argelem / argdefelem under null(ex-argcheck), or
/// (perl 5.44+) multiparam with paramtest/paramstore for defaults
fn detect_signature(ir: &SubIr) -> Option<ArgSpec> {
    let mut check: Option<(u64, u64, Option<char>)> = None;
    let mut multi: Option<&OpDetail> = None;
    let mut params: Vec<Param> = Vec::new();
    // multiparam defaults: paramtest (targ = padix) -> paramstore -> expr
    let mut defaults: HashMap<u64, String> = HashMap::new();

    super::walk_with_lines(ir, |node, _line| match &node.detail {
        OpDetail::ArgCheck {
            params: p,
            opt,
            slurpy,
        } => {
            check = Some((*p, *opt, *slurpy));
        }
        OpDetail::ArgElem { index } => {
            // the default expression is the child of the argdefelem under the argelem
            let default = node
                .kids
                .first()
                .map(|k| k.skip_null())
                .filter(|k| k.name == "argdefelem")
                .and_then(|d| d.kids.first())
                .map(|e| render(ir, e));
            params.push(Param {
                name: ir.pad_name(node.targ).map(String::from),
                index: *index,
                default,
                source: "signature".into(),
                key: None,
                ..Param::new()
            });
        }
        OpDetail::MultiParam { .. } => multi = Some(&node.detail),
        _ if node.name == "paramtest" => {
            if let Some(expr) = node
                .kids
                .first()
                .filter(|k| k.name == "paramstore")
                .and_then(|st| st.kids.first())
            {
                defaults.insert(node.targ, render(ir, expr));
            }
        }
        _ => {}
    });

    if let Some(OpDetail::MultiParam {
        min_args,
        n_positional,
        slurpy,
        param_padix,
        slurpy_padix,
        named,
    }) = multi
    {
        return Some(multiparam_spec(
            ir,
            *min_args,
            *n_positional,
            *slurpy,
            param_padix,
            *slurpy_padix,
            named,
            &defaults,
        ));
    }

    let (p, opt, slurpy) = check?;
    params.sort_by_key(|x| x.index);
    Some(ArgSpec {
        style: "signature".into(),
        min_arity: p - opt,
        max_arity: if slurpy.is_some() { None } else { Some(p) },
        invocant_guess: guess_invocant(&params),
        params,
        ..ArgSpec::empty()
    })
}

/// ArgSpec of a perl 5.44+ multiparam signature. Like argelem on older
/// perls, unnamed placeholders (pad index 0) yield no Param. Each required
/// named parameter adds a key/value pair (2) to the minimum arity; named
/// parameters lift the upper bound like a slurpy does.
#[allow(clippy::too_many_arguments)]
fn multiparam_spec(
    ir: &SubIr,
    min_args: u64,
    n_positional: u64,
    slurpy: Option<char>,
    param_padix: &[u64],
    slurpy_padix: u64,
    named: &[NamedParam],
    defaults: &HashMap<u64, String>,
) -> ArgSpec {
    let pad_param = |padix: u64, index: u64, key: Option<String>| Param {
        name: ir.pad_name(padix).map(String::from),
        index,
        default: defaults.get(&padix).cloned(),
        source: "signature".into(),
        key,
        ..Param::new()
    };
    let mut params: Vec<Param> = param_padix
        .iter()
        .enumerate()
        .filter(|(_, padix)| **padix != 0)
        .map(|(i, padix)| pad_param(*padix, i as u64, None))
        .collect();
    if slurpy.is_some() && slurpy_padix != 0 {
        params.push(pad_param(slurpy_padix, n_positional, None));
    }
    for n in named {
        params.push(pad_param(n.padix, n_positional, Some(n.name.clone())));
    }
    let required_named = named.iter().filter(|n| n.required).count() as u64;
    ArgSpec {
        style: "signature".into(),
        min_arity: min_args + 2 * required_named,
        max_arity: if slurpy.is_some() || !named.is_empty() {
            None
        } else {
            Some(n_positional)
        },
        invocant_guess: guess_invocant(&params),
        params,
        ..ArgSpec::empty()
    }
}

impl ArgSpec {
    fn empty() -> ArgSpec {
        ArgSpec {
            style: "none".into(),
            min_arity: 0,
            max_arity: None,
            params: Vec::new(),
            invocant_guess: None,
            named: Vec::new(),
            forwards: Vec::new(),
            helpers: Vec::new(),
            checks: Vec::new(),
            notes: Vec::new(),
        }
    }
}

impl Param {
    fn new() -> Param {
        Param {
            name: None,
            index: 0,
            default: None,
            source: String::new(),
            key: None,
            alts: Vec::new(),
            line: None,
            conditional: false,
            typ: None,
        }
    }
}

/// The argument slots a value holds (hash keys excluded: those are
/// reported as [`NamedKey`]s).
fn slots_of(v: &Sym) -> Vec<Slot> {
    let mut out = Vec::new();
    collect_slots(v, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_slots(v: &Sym, out: &mut Vec<Slot>) {
    match v {
        Sym::Arg(i) => out.push(Slot::Front(*i as u64)),
        Sym::ArgBack(i) => out.push(Slot::Back(*i as u64)),
        Sym::ArgRest(i) | Sym::ArgMid(i, _) => out.push(Slot::Rest(*i as u64)),
        Sym::Hash { segs } => {
            for seg in segs {
                if let symexec::HashSeg::Rest(r) = seg {
                    if let Some((f, _)) = r.as_arg_slice() {
                        out.push(Slot::Rest(f as u64));
                    }
                }
            }
        }
        Sym::Default { arg, .. } => collect_slots(arg, out),
        Sym::Join(alts) => alts.iter().for_each(|(_, s)| collect_slots(s, out)),
        _ => {}
    }
}

/// Helpers whose calls carry type rules after each declared variable
/// (`args my $x => 'Int', my $y => { isa => 'Str' }`), by name.
const TYPED_HELPERS: &[&str] = &["args", "args_pos"];

/// `(variable, type)` from the rules at the calls of [`TYPED_HELPERS`] that
/// were executed. The rules are constants the helper reads but does not
/// return, so they are read off the call.
fn helper_hints(ir: &SubIr, sum: &Summary) -> Vec<(String, String)> {
    let hinted = |name: &str| TYPED_HELPERS.contains(&name.rsplit("::").next().unwrap_or(name));
    if !sum.calls.iter().any(|c| c.inlined && hinted(&c.callee)) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for c in super::callsites::call_sites(ir) {
        let super::callsites::Callee::Named { name } = &c.callee else {
            continue;
        };
        if !hinted(name) {
            continue;
        }
        let Some(node) = ir.node_by_id(c.node) else {
            continue;
        };
        let kids: Vec<&crate::ir::OpNode> = match node.kids.as_slice() {
            [only] if only.op_type == 0 && !only.kids.is_empty() => only.kids.iter().collect(),
            ks => ks.iter().collect(),
        };
        let kids: Vec<&crate::ir::OpNode> = kids
            .into_iter()
            .filter(|k| !matches!(k.name.as_str(), "pushmark" | "padrange"))
            .filter(|k| !(k.op_type == 0 && k.kids.is_empty()))
            .collect();
        let mut var: Option<String> = None;
        for k in kids {
            let k = k.skip_null();
            match (k.name.as_str(), &k.detail) {
                ("padsv", _) => var = ir.pad_name(k.targ).map(String::from),
                ("const", OpDetail::Const(crate::ir::SvLit::Pv(t))) => {
                    if let Some(v) = var.take() {
                        out.push((v, t.clone()));
                    }
                }
                ("anonhash", _) => {
                    let items: Vec<&crate::ir::OpNode> =
                        k.kids.iter().map(|x| x.skip_null()).filter(|x| x.name != "pushmark").collect();
                    let isa = items.chunks(2).find_map(|kv| match (kv.first(), kv.get(1)) {
                        (Some(a), Some(b)) => match (&a.detail, &b.detail) {
                            (
                                OpDetail::Const(crate::ir::SvLit::Pv(key)),
                                OpDetail::Const(crate::ir::SvLit::Pv(t)),
                            ) if key == "isa" || key == "does" => Some(t.clone()),
                            _ => None,
                        },
                        _ => None,
                    });
                    if let (Some(v), Some(t)) = (var.take(), isa) {
                        out.push((v, t));
                    }
                }
                _ => var = None,
            }
        }
    }
    out
}

/// Keys a value was matched under (`Key::Any(Matches)` elements of an
/// argument-derived hash), in order, without duplicates.
fn collect_matches(v: &Sym, out: &mut Vec<(String, Vec<Norm>)>) {
    match v {
        Sym::Elem(base, Key::Any(symexec::Constraint::Matches { norm, eq })) if base.mentions_args() => {
            if !out.iter().any(|(k, _)| k == eq) {
                out.push((eq.clone(), norm.clone()));
            }
        }
        Sym::Elem(base, Key::Str(k)) if base.mentions_args() => {
            if !out.iter().any(|(x, _)| x == k) {
                out.push((k.clone(), Vec::new()));
            }
        }
        Sym::Default { arg, .. } => collect_matches(arg, out),
        Sym::Join(alts) => alts.iter().for_each(|(_, s)| collect_matches(s, out)),
        _ => {}
    }
}

/// `scalar(@_)` (of what remains of the arguments).
fn is_count(v: &Sym) -> bool {
    matches!(v, Sym::Len(l) if arity_offset(l).is_some())
}

/// The value a parameter is described by: the value at its first
/// assignment, or the final value when that only adds a default or
/// alternatives to it (`$x //= 1`, `$opt = shift if ...`).
fn choose(first: &Sym, last: Option<&Sym>) -> Sym {
    let Some(last) = last else {
        return first.clone();
    };
    let extends = |s: &Sym| s == first || matches!(s, Sym::Default { arg, .. } if **arg == *first);
    match last {
        s if extends(s) => s.clone(),
        Sym::Join(alts)
            if alts.iter().any(|(_, s)| extends(s))
                && alts
                    .iter()
                    .all(|(_, s)| extends(s) || s.is_const() || !slots_of(s).is_empty()) =>
        {
            last.clone()
        }
        _ => first.clone(),
    }
}

fn default_of(v: &Sym) -> Option<String> {
    match v {
        Sym::Default { default, .. } => Some(default.display()),
        Sym::Join(alts) => alts
            .iter()
            .find_map(|(_, s)| default_of(s))
            .or_else(|| {
                // a constant taken when the argument is missing
                alts.iter().find_map(|(conds, s)| {
                    let missing = conds.iter().any(|c| {
                        !c.truth && matches!(&c.sym, Sym::App("exists" | "defined", _) | Sym::Len(_))
                    });
                    (missing && s.is_const() && *s != Sym::undef()).then(|| s.display())
                })
            }),
        _ => None,
    }
}

/// The key of a hash-argument read (`$args{key}` → `key`).
fn named_key(v: &Sym) -> Option<&str> {
    match v {
        Sym::Elem(base, Key::Str(k)) if base.mentions_args() => Some(k),
        Sym::Default { arg, .. } => named_key(arg),
        Sym::Join(alts) => alts.iter().find_map(|(_, s)| named_key(s)),
        _ => None,
    }
}

/// A callee name as written in the sub's package.
fn local_name(ir: &SubIr, name: &str) -> String {
    let pkg = ir.package_name();
    name.strip_prefix(pkg)
        .and_then(|r| r.strip_prefix("::"))
        .filter(|r| !r.contains("::"))
        .or_else(|| name.strip_prefix("main::"))
        .unwrap_or(name)
        .to_string()
}

/// An arity precondition as `(cmp, n)` on the original argument count.
fn arity_cond(c: &Cond) -> Option<(&'static str, i64)> {
    match &c.sym {
        Sym::Len(l) => {
            let off = arity_offset(l)?;
            Some(if c.truth { (">=", off + 1) } else { ("==", off) })
        }
        Sym::App(op, v) if v.len() == 2 => {
            let (op, l, k) = match (&v[0], &v[1]) {
                (Sym::Len(l), k) => (*op, l, k),
                (k, Sym::Len(l)) => (flip(op)?, l, k),
                _ => return None,
            };
            let n = k.const_int()? + arity_offset(l)?;
            let op = if c.truth { op } else { negate(op)? };
            matches!(op, "==" | "!=" | "<" | ">" | "<=" | ">=").then_some((op, n))
        }
        _ => None,
    }
}

fn flip(op: &str) -> Option<&'static str> {
    Some(match op {
        "==" => "==",
        "!=" => "!=",
        "<" => ">",
        ">" => "<",
        "<=" => ">=",
        ">=" => "<=",
        _ => return None,
    })
}

fn negate(op: &str) -> Option<&'static str> {
    Some(match op {
        "==" => "!=",
        "!=" => "==",
        "<" => ">=",
        ">" => "<=",
        "<=" => ">",
        ">=" => "<",
        _ => return None,
    })
}

/// The spec from a symbolic execution of the body.
fn spec_from_summary(ir: &SubIr, sum: &Summary) -> ArgSpec {
    let fin = &sum.env.top().pads;
    // (seq, param, value it stands for)
    let mut params: Vec<(u32, Param, Sym)> = Vec::new();

    // variables that receive arguments
    for a in &sum.assigns {
        let Some(entry) = ir.pad.iter().find(|e| e.ix as u64 == a.targ) else {
            continue;
        };
        let Some(name) = entry.name.clone() else {
            continue;
        };
        let value = choose(&a.sym, fin.get(&a.targ));
        let mut alts = slots_of(&value);
        if alts.is_empty() {
            if !is_count(&value) {
                continue;
            }
            alts.push(Slot::Count);
        }
        params.push((
            a.seq,
            Param {
                name: Some(name),
                default: default_of(&value),
                source: a.source.into(),
                alts,
                line: a.line,
                conditional: a.conditional || matches!(value, Sym::Join(_)),
                typ: entry.typ.clone(),
                ..Param::new()
            },
            value,
        ));
    }

    // direct element reads and loops over the arguments
    let mut elem_bound: u64 = 0;
    for r in &sum.reads {
        match &r.kind {
            ReadKind::Elem => {
                if let (Sym::Arg(i), false) = (&r.sym, r.conditional) {
                    elem_bound = elem_bound.max(*i as u64 + 1);
                }
                let alts = slots_of(&r.sym);
                if alts.is_empty() || params.iter().any(|(_, _, v)| *v == r.sym) {
                    continue;
                }
                params.push((
                    r.seq,
                    Param {
                        name: r.text.clone(),
                        source: "elem".into(),
                        alts,
                        line: r.line,
                        conditional: r.conditional || matches!(r.sym, Sym::Join(_)),
                        ..Param::new()
                    },
                    r.sym.clone(),
                ));
            }
            ReadKind::Iter => {
                let alts = slots_of(&r.sym);
                if params
                    .iter()
                    .any(|(_, p, _)| p.alts.iter().any(|s| alts.contains(s)))
                {
                    continue;
                }
                params.push((
                    r.seq,
                    Param {
                        name: Some("@_".into()),
                        source: "list".into(),
                        alts,
                        line: r.line,
                        conditional: r.conditional,
                        ..Param::new()
                    },
                    r.sym.clone(),
                ));
            }
            _ => {}
        }
    }
    params.sort_by_key(|(seq, _, _)| *seq);

    // keys a helper matched against the arguments (`rearrange`): each
    // variable's matched keys are one parameter, the first its key and
    // the others aliases
    let helper_via = sum
        .calls
        .iter()
        .find(|c| c.inlined)
        .map(|c| local_name(ir, &c.callee));
    let mut matched_named: Vec<NamedKey> = Vec::new();
    for a in &sum.assigns {
        let v = choose(&a.sym, fin.get(&a.targ));
        let mut keys: Vec<(String, Vec<Norm>)> = Vec::new();
        collect_matches(&v, &mut keys);
        let Some(((first, norm), rest)) = keys.split_first() else {
            continue;
        };
        if matched_named.iter().any(|n| n.key == *first) {
            continue;
        }
        matched_named.push(NamedKey {
            key: first.clone(),
            via: helper_via.clone(),
            default: default_of(&v),
            required: false,
            line: a.line,
            normalize: norm.clone(),
            aliases: rest.iter().map(|(k, _)| k.clone()).collect(),
            typ: None,
        });
    }
    let positional = params
        .iter()
        .flat_map(|(_, p, _)| &p.alts)
        .filter_map(|s| match s {
            Slot::Front(n) => Some(n + 1),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let mut params: Vec<Param> = params.into_iter().map(|(_, p, _)| p).collect();
    for p in &mut params {
        p.index = match p.alts.first() {
            Some(Slot::Front(n) | Slot::Rest(n)) => *n,
            _ => positional,
        };
    }

    // keys of hash arguments
    let mut named: Vec<NamedKey> = Vec::new();
    let mut plain: Vec<String> = Vec::new();
    let mut tested: Vec<String> = Vec::new();
    for r in &sum.reads {
        if matches!(r.kind, ReadKind::Elem | ReadKind::Iter) {
            continue;
        }
        let Some(key) = named_key(&r.sym) else {
            continue;
        };
        let ix = match named.iter().position(|n| n.key == key) {
            Some(ix) => ix,
            None => {
                named.push(NamedKey {
                    key: key.to_string(),
                    via: None,
                    default: None,
                    required: false,
                    line: r.line,
                    normalize: Vec::new(),
                    aliases: Vec::new(),
                    typ: None,
                });
                named.len() - 1
            }
        };
        let n = &mut named[ix];
        if n.via.is_none() {
            n.via = r.via.clone();
        }
        match &r.kind {
            ReadKind::Default(d) if n.default.is_none() => n.default = Some(d.display()),
            ReadKind::Key if !r.conditional => plain.push(key.to_string()),
            ReadKind::Exists => tested.push(key.to_string()),
            _ => {}
        }
    }
    for n in &mut named {
        n.required = n.default.is_none() && plain.contains(&n.key) && !tested.contains(&n.key);
    }
    for m in matched_named {
        if !named.iter().any(|n| n.key == m.key) {
            named.push(m);
        }
    }
    // a key whose absence dies is required
    for p in sum.preconds.iter().filter(|p| p.enforced && !p.conditional && p.cond.truth) {
        if let Sym::App("exists", x) = &p.cond.sym {
            if let Some(k) = x.first().and_then(named_key) {
                if let Some(n) = named.iter_mut().find(|n| n.key == k) {
                    n.required = true;
                }
            }
        }
    }

    // arity checks
    let mut checks = Vec::new();
    let (mut lo, mut hi): (u64, Option<u64>) = (0, None);
    for p in sum.preconds.iter().filter(|p| !p.conditional) {
        let Some((cmp, n)) = arity_cond(&p.cond) else {
            continue;
        };
        if n < 0 {
            continue;
        }
        let n = n as u64;
        if p.enforced {
            match cmp {
                "==" => {
                    lo = lo.max(n);
                    hi = Some(hi.map_or(n, |h| h.min(n)));
                }
                ">=" => lo = lo.max(n),
                ">" => lo = lo.max(n + 1),
                "<=" => hi = Some(hi.map_or(n, |h| h.min(n))),
                "<" if n > 0 => hi = Some(hi.map_or(n - 1, |h| h.min(n - 1))),
                _ => {}
            }
        }
        checks.push(ArityCheck {
            line: p.line,
            cmp: cmp.into(),
            n: Some(n),
            enforced: p.enforced,
        });
    }

    let forwards: Vec<Forward> = sum
        .forwards
        .iter()
        .map(|f| Forward {
            callee: if f.method {
                f.callee.clone()
            } else {
                local_name(ir, &f.callee)
            },
            line: f.line,
            kind: f.kind.into(),
            from: f.from,
        })
        .collect();

    let mut styles: Vec<&str> = Vec::new();
    for p in &params {
        let st = match p.source.as_str() {
            "shift" | "pop" | "splice" => "shift",
            "unpack" => "unpack",
            "elem" => "positional",
            "helper" => "helper",
            _ => continue,
        };
        if !styles.contains(&st) {
            styles.push(st);
        }
    }
    let style = match styles.as_slice() {
        [] => "none".to_string(),
        [one] => one.to_string(),
        _ => "mixed".to_string(),
    };
    // an upper bound from the unpacking itself: `my (...) = @_` without a
    // slurpy, and nothing else that touches the arguments
    let bounded = styles == ["unpack"]
        && forwards.is_empty()
        && params
            .iter()
            .all(|p| p.alts.iter().all(|s| matches!(s, Slot::Front(_))));
    let mut max_arity = bounded.then_some(positional);
    if let Some(h) = hi {
        max_arity = Some(max_arity.map_or(h, |m| m.min(h)));
    }
    let notes = sum.notes.clone();
    for (var, typ) in helper_hints(ir, sum) {
        if let Some(n) = named.iter_mut().find(|n| n.key == var[1..]) {
            n.typ.get_or_insert(typ.clone());
        }
        if let Some(p) = params.iter_mut().find(|p| p.name.as_deref() == Some(var.as_str())) {
            p.typ.get_or_insert(typ);
        }
    }
    ArgSpec {
        style,
        min_arity: lo.max(elem_bound),
        max_arity,
        invocant_guess: guess_invocant(&params),
        params,
        named,
        forwards,
        helpers: sum
            .calls
            .iter()
            .filter(|c| c.inlined)
            .map(|c| HelperUse {
                callee: local_name(ir, &c.callee),
                line: c.line,
                inlined: true,
                note: None,
            })
            .collect(),
        checks,
        notes,
    }
}
/// The argument spec without symbolic execution: a signature, or else
/// the leading run of argument-extraction statements of the body (`my
/// (...) = @_`, `my $x = shift`) and the `$_[n]` accesses anywhere in it
/// (the analysis before [`super::symexec`]). Cheap and bounded by the
/// size of the body, for callers that cannot afford the symbolic
/// execution or need to avoid it.
pub fn analyze_args_preamble(ir: &SubIr) -> ArgSpec {
    if let Some(spec) = detect_signature(ir) {
        return spec;
    }
    let mut params: Vec<Param> = Vec::new();
    let mut styles: Vec<&str> = Vec::new();
    let mut slurpy = false;
    let mut next_index: u64 = 0;

    // the preamble: the leading run of argument-extraction statements
    for (line, stmt) in super::statements(ir) {
        let s = stmt.skip_null();
        if let Some(name) = preamble::shift_into_pad(ir, s) {
            params.push(Param {
                name: Some(name),
                index: next_index,
                source: "shift".into(),
                alts: vec![Slot::Front(next_index)],
                line,
                ..Param::new()
            });
            next_index += 1;
            if !styles.contains(&"shift") {
                styles.push("shift");
            }
            continue;
        }
        if let Some((names, has_slurpy)) = preamble::unpack_from_args(ir, s) {
            for name in names {
                let rest = name.starts_with('@') || name.starts_with('%');
                params.push(Param {
                    name: Some(name),
                    index: next_index,
                    source: "unpack".into(),
                    alts: vec![if rest {
                        Slot::Rest(next_index)
                    } else {
                        Slot::Front(next_index)
                    }],
                    line,
                    ..Param::new()
                });
                if !rest {
                    next_index += 1;
                }
            }
            slurpy = slurpy || has_slurpy;
            if !styles.contains(&"unpack") {
                styles.push("unpack");
            }
            continue;
        }
        break; // a statement that is not argument extraction ends the preamble
    }

    // direct $_[n] access (whole tree)
    let mut max_elem: Option<(i64, Option<u32>)> = None;
    super::walk_with_lines(ir, |node, line| {
        if let Some(ix) = preamble::arg_elem_index(node) {
            if max_elem.is_none_or(|(m, _)| ix > m) {
                max_elem = Some((ix, line));
            }
        }
    });
    if let Some((m, line)) = max_elem.filter(|(m, _)| *m >= 0) {
        if !styles.contains(&"positional") {
            styles.push("positional");
        }
        for ix in 0..=m as u64 {
            if !params.iter().any(|p| p.index == ix) {
                params.push(Param {
                    index: ix,
                    source: "elem".into(),
                    alts: vec![Slot::Front(ix)],
                    line,
                    ..Param::new()
                });
            }
        }
    }

    let style = match styles.as_slice() {
        [] => "none".to_string(),
        [one] => one.to_string(),
        _ => "mixed".to_string(),
    };
    let positional = params
        .iter()
        .filter(|p| p.name.as_deref().is_none_or(|n| !n.starts_with('@') && !n.starts_with('%')))
        .count() as u64;
    ArgSpec {
        style,
        // these patterns cannot tell required from optional: only a
        // `$_[n]` access gives grounds for a lower bound
        min_arity: max_elem.map_or(0, |(m, _)| (m + 1).max(0) as u64),
        // an upper bound is only known for an unpack without a slurpy
        max_arity: (!slurpy && styles == ["unpack"]).then_some(positional),
        invocant_guess: guess_invocant(&params),
        params,
        notes: vec!["preamble only (no symbolic execution)".into()],
        ..ArgSpec::empty()
    }
}

/// The statement patterns of [`analyze_args_preamble`].
mod preamble {
    use crate::ir::{OpDetail, OpNode, SubIr, SvLit};

    /// `my $x = shift;` → padsv_store(targ=$x) with a bare shift as its
    /// child (the older form sassign(shift, padsv) is also handled)
    pub(super) fn shift_into_pad(ir: &SubIr, s: &OpNode) -> Option<String> {
        match s.name.as_str() {
            "padsv_store" => {
                let k = s.kids.first()?.skip_null();
                (k.name == "shift" && shifts_args(k)).then(|| ir.pad_name(s.targ).map(String::from))?
            }
            "sassign" => {
                let rhs = s.kids.first()?.skip_null();
                let lhs = s.kids.get(1)?.skip_null();
                (rhs.name == "shift" && shifts_args(rhs) && lhs.name == "padsv")
                    .then(|| ir.pad_name(lhs.targ).map(String::from))?
            }
            _ => None,
        }
    }

    /// The shift is on `@_` (bare, or `shift @_`).
    fn shifts_args(shift: &OpNode) -> bool {
        shift.kids.is_empty() || has_args_av(shift)
    }

    /// `my (...) = @_;` → aassign(rv2av(*_) on the RHS, pad variables on
    /// the LHS): the names, and whether one of them is an array / hash
    pub(super) fn unpack_from_args(ir: &SubIr, s: &OpNode) -> Option<(Vec<String>, bool)> {
        if s.name != "aassign" {
            return None;
        }
        let (rhs, lhs) = (s.kids.first()?, s.kids.get(1)?);
        if !has_args_av(rhs) {
            return None;
        }
        let mut names = Vec::new();
        let mut slurpy = false;
        lhs_targets(ir, lhs, &mut names, &mut slurpy);
        (!names.is_empty()).then_some((names, slurpy))
    }

    fn lhs_targets(ir: &SubIr, n: &OpNode, names: &mut Vec<String>, slurpy: &mut bool) {
        let n = n.skip_null();
        match n.name.as_str() {
            "padsv" | "padav" | "padhv" => {
                if let Some(name) = ir.pad_name(n.targ) {
                    names.push(name.to_string());
                    *slurpy |= n.name != "padsv";
                }
            }
            _ => n.kids.iter().for_each(|k| lhs_targets(ir, k, names, slurpy)),
        }
    }

    fn is_args_gv(n: &OpNode) -> bool {
        matches!(&n.detail, OpDetail::Gv { name, stash }
            if name == "_" && stash.as_deref().unwrap_or("main") == "main")
    }

    /// The subtree contains `@_` (rv2av of gv *_)
    fn has_args_av(n: &OpNode) -> bool {
        let n = n.skip_null();
        (n.name == "rv2av" && n.kids.first().is_some_and(|k| is_args_gv(k.skip_null())))
            || n.kids.iter().any(has_args_av)
    }

    /// `$_[n]`: the index is the op_private of aelemfast (gv *_); the
    /// aelem(rv2av(*_), const n) form is also covered
    pub(super) fn arg_elem_index(n: &OpNode) -> Option<i64> {
        // nulls are not skipped: the aelemfast under ex-aelem is the real op
        if n.name == "aelemfast" && is_args_gv(n) {
            return Some(n.private as i8 as i64);
        }
        if n.name == "aelem" && n.kids.len() == 2 && has_args_av(&n.kids[0]) {
            if let OpDetail::Const(SvLit::Iv(ix)) = &n.kids[1].skip_null().detail {
                return Some(*ix);
            }
        }
        None
    }
}

fn guess_invocant(params: &[Param]) -> Option<String> {
    let first = params.iter().find(|p| p.index == 0)?;
    let name = first.name.as_deref()?;
    matches!(name, "$self" | "$class" | "$proto" | "$pkg").then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passes::test_util::ir_of;

    macro_rules! golden {
        ($name:literal) => {
            include_str!(concat!("../../../../t/golden/5.42-threaded/", $name, ".json"))
        };
    }

    fn spec(golden: &str) -> ArgSpec {
        let s = analyze_args(&ir_of(golden));
        eprintln!("{}", serde_json::to_string_pretty(&s).unwrap());
        s
    }

    fn param<'a>(s: &'a ArgSpec, name: &str) -> &'a Param {
        s.params
            .iter()
            .find(|p| p.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("no param {}", name))
    }

    fn names(s: &ArgSpec) -> Vec<&str> {
        s.params.iter().filter_map(|p| p.name.as_deref()).collect()
    }

    fn key<'a>(s: &'a ArgSpec, k: &str) -> &'a NamedKey {
        s.named
            .iter()
            .find(|n| n.key == k)
            .unwrap_or_else(|| panic!("no key {}", k))
    }

    #[test]
    fn shift_then_loop_over_the_rest() {
        let s = spec(golden!("args_shift_loop"));
        assert_eq!(names(&s), ["$sum", "@_"]);
        assert_eq!(param(&s, "$sum").alts, [Slot::Front(0)]);
        assert_eq!(param(&s, "$sum").source, "shift");
        assert_eq!(param(&s, "@_").alts, [Slot::Rest(1)]);
        assert_eq!(param(&s, "@_").source, "list");
        assert_eq!(s.style, "shift");
    }

    #[test]
    fn hash_of_options() {
        let s = spec(golden!("args_opts_hash"));
        assert_eq!(names(&s), ["$self", "%args"]);
        assert_eq!(param(&s, "$self").alts, [Slot::Front(0)]);
        assert_eq!(param(&s, "%args").alts, [Slot::Rest(1)]);
        assert_eq!(s.invocant_guess.as_deref(), Some("$self"));
        let foo = key(&s, "foo");
        assert_eq!(foo.default.as_deref(), Some("\"dflt\""));
        assert_eq!(foo.via.as_deref(), Some("%args"));
        assert!(!foo.required);
        assert!(key(&s, "bar").required);
        assert!(!key(&s, "baz").required);
        assert_eq!(s.max_arity, None);
    }

    #[test]
    fn hash_with_default_pairs() {
        let s = spec(golden!("args_hash_defaults"));
        assert_eq!(param(&s, "%args").alts, [Slot::Rest(0)]);
        assert_eq!(key(&s, "color").default.as_deref(), Some("\"red\""));
        assert!(key(&s, "size").required);
    }

    #[test]
    fn hashref_argument() {
        let s = spec(golden!("args_hashref"));
        assert_eq!(param(&s, "$opts").alts, [Slot::Front(1)]);
        let v = key(&s, "verbose");
        assert_eq!(v.via.as_deref(), Some("$opts"));
        assert_eq!(v.default.as_deref(), Some("0"));
    }

    #[test]
    fn pop_splice_and_count() {
        let s = spec(golden!("args_pop_splice"));
        assert_eq!(names(&s), ["$last", "$first", "$n"]);
        assert_eq!(param(&s, "$last").alts, [Slot::Back(0)]);
        assert_eq!(param(&s, "$first").alts, [Slot::Front(0)]);
        assert_eq!(param(&s, "$n").alts, [Slot::Count]);
    }

    #[test]
    fn five_forms_of_default() {
        let s = spec(golden!("args_defaults"));
        for (i, (name, d)) in [("$a", "da"), ("$b", "db"), ("$c", "dc"), ("$d", "dd"), ("$e", "de")]
            .into_iter()
            .enumerate()
        {
            let p = param(&s, name);
            assert_eq!(p.alts, [Slot::Front(i as u64)], "{}", name);
            assert_eq!(p.source, "shift", "{}", name);
            assert_eq!(p.default.as_deref(), Some(format!("\"{}\"", d).as_str()), "{}", name);
        }
    }

    #[test]
    fn forwards() {
        let s = spec(golden!("args_forward"));
        let f: Vec<(&str, &str, u32)> = s
            .forwards
            .iter()
            .map(|f| (f.callee.as_str(), f.kind.as_str(), f.from))
            .collect();
        assert_eq!(
            f,
            [
                ("SUPER::new", "rest", 1),
                ("other", "rest", 1),
                ("other2", "amper", 1),
                ("other3", "goto", 1)
            ]
        );
    }

    #[test]
    fn arity_checks() {
        let s = spec(golden!("args_checks"));
        let c: Vec<(&str, Option<u64>, bool)> = s
            .checks
            .iter()
            .map(|c| (c.cmp.as_str(), c.n, c.enforced))
            .collect();
        assert_eq!(c, [("==", Some(2), true), ("<=", Some(5), true)]);
        assert_eq!((s.min_arity, s.max_arity), (2, Some(2)));
    }

    #[test]
    fn conditional_shift() {
        let s = spec(golden!("args_conditional"));
        assert_eq!(names(&s), ["$x", "$opt", "$z"]);
        let x = param(&s, "$x");
        assert_eq!((x.alts.as_slice(), x.conditional), (&[Slot::Front(0)][..], false));
        let opt = param(&s, "$opt");
        assert_eq!((opt.alts.as_slice(), opt.conditional), (&[Slot::Front(1)][..], true));
        let z = param(&s, "$z");
        assert_eq!(z.alts, [Slot::Front(1), Slot::Front(2)]);
        assert!(z.conditional);
    }

    #[test]
    fn elements_after_shift() {
        let s = spec(golden!("args_elem_after_shift"));
        assert_eq!(names(&s), ["$first", "$_[0]", "$_[1]"]);
        assert_eq!(param(&s, "$_[0]").alts, [Slot::Front(1)]);
        assert_eq!(param(&s, "$_[1]").alts, [Slot::Front(2)]);
        assert_eq!(s.min_arity, 3);
    }

    /// Supplies the helper fixtures' bodies by qualified name.
    struct Helpers(Vec<(&'static str, std::sync::Arc<SubIr>)>);

    impl IrSource for Helpers {
        fn sub_ir(&self, callee: &str, _: &str) -> Option<std::sync::Arc<SubIr>> {
            self.0.iter().find(|(n, _)| *n == callee).map(|(_, ir)| ir.clone())
        }
        fn canonical_name(&self, _: &str, _: &str) -> Option<String> {
            None
        }
        fn outer_value(&self, _: &str, _: u32) -> Option<Sym> {
            None
        }
    }

    fn m4i_helpers() -> Helpers {
        let ir = |g: &str| std::sync::Arc::new(ir_of(g));
        Helpers(vec![
            ("Fixtures::m4i_args", ir(golden!("args_helper_m4i_args"))),
            ("Fixtures::m4i_opts", ir(golden!("args_helper_m4i_opts"))),
            ("Fixtures::m4i_opts_alike", ir(golden!("args_helper_m4i_opts_alike"))),
        ])
    }

    #[test]
    fn helper_m4i_args_inlined() {
        // my ($pack, $opts, @rest) = m4i_args(@_); $opts->{destpkg} ...
        let ir = ir_of(golden!("args_helper_m4i"));
        let s = analyze_args_with(&ir, &m4i_helpers(), &Limits::default());
        eprintln!("{}", serde_json::to_string_pretty(&s).unwrap());
        assert_eq!(s.style, "helper");
        assert_eq!(names(&s), ["$pack", "$opts", "@rest"]);
        assert_eq!(param(&s, "$pack").alts, [Slot::Front(0)]);
        assert_eq!(param(&s, "$opts").alts, [Slot::Front(1)]);
        assert!(param(&s, "$opts").conditional, "an Opts object or $_[1] itself");
        assert_eq!(param(&s, "@rest").alts, [Slot::Rest(2)]);
        let h: Vec<(&str, bool)> = s.helpers.iter().map(|h| (h.callee.as_str(), h.inlined)).collect();
        assert_eq!(h, [("m4i_args", true)]);
        assert!(s.forwards.is_empty());
        assert_eq!(key(&s, "destpkg").via.as_deref(), Some("$opts"));
    }

    fn rearrange_helpers() -> Helpers {
        let ir = |g: &str| std::sync::Arc::new(ir_of(g));
        Helpers(vec![
            ("Fixtures::rearrange", ir(golden!("args_helper_rearrange_fn"))),
            ("Fixtures::_rearrange_params", ir(golden!("args_helper_rearrange_params"))),
        ])
    }

    #[test]
    fn helper_rearrange_inlined() {
        // my ($name, $value) = rearrange(['NAME', ['VALUE', 'VALUES']], @_)
        let ir = ir_of(golden!("args_helper_rearrange"));
        let s = analyze_args_with(&ir, &rearrange_helpers(), &Limits::default());
        eprintln!("{}", serde_json::to_string_pretty(&s).unwrap());
        assert_eq!(s.style, "helper");
        assert_eq!(param(&s, "$name").alts, [Slot::Front(0)]);
        assert_eq!(param(&s, "$value").alts, [Slot::Front(1)]);
        let name = key(&s, "name");
        assert_eq!(name.normalize, [Norm::Lc, Norm::StripPrefix("-".into())]);
        let value = key(&s, "value");
        assert_eq!(value.aliases, ["values"]);
    }

    #[test]
    fn helper_smart_args_inlined() {
        // args my $self, my $x => 'Int', my $y => { isa => 'Str', optional => 1 }, my $z;
        let ir = |g: &str| std::sync::Arc::new(ir_of(g));
        let src = Helpers(vec![
            ("Fixtures::args", ir(golden!("args_helper_smart_args"))),
            ("Fixtures::_validate_by_rule", ir(golden!("args_helper_smart_validate"))),
        ]);
        let user = ir_of(golden!("args_helper_smart"));
        let s = analyze_args_with(&user, &src, &Limits::default());
        eprintln!("{}", serde_json::to_string_pretty(&s).unwrap());
        assert_eq!(s.style, "helper");
        assert_eq!(names(&s), ["$self"]);
        assert_eq!(param(&s, "$self").alts, [Slot::Front(0)]);
        assert_eq!(s.invocant_guess.as_deref(), Some("$self"));
        assert!(key(&s, "x").required);
        assert!(!key(&s, "y").required);
        assert!(key(&s, "z").required);
        assert_eq!(key(&s, "x").typ.as_deref(), Some("Int"));
        assert_eq!(key(&s, "y").typ.as_deref(), Some("Str"));
        assert_eq!(key(&s, "w").default.as_deref(), Some("3"));
        assert!(!key(&s, "w").required);
        assert_eq!(key(&s, "x").line, Some(2), "the line of the call");
        assert_eq!(param(&s, "$self").line, Some(2));
    }

    #[test]
    fn helper_without_source_is_a_forward() {
        let s = spec(golden!("args_helper_m4i"));
        assert_eq!(s.style, "none");
        assert_eq!(s.forwards.len(), 1);
        assert_eq!((s.forwards[0].callee.as_str(), s.forwards[0].kind.as_str()), ("m4i_args", "all"));
    }

    #[test]
    fn classic_styles_keep_their_shape() {
        let s = spec(golden!("unpack_args"));
        assert_eq!(s.style, "unpack");
        let s = spec(golden!("shift_style"));
        assert_eq!(s.style, "shift");
    }

    #[test]
    fn preamble_matches_the_leading_statements() {
        let pre = |g: &str| analyze_args_preamble(&ir_of(g));
        let names = |s: &ArgSpec| s.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        for g in [golden!("unpack_args"), golden!("shift_style")] {
            let (p, full) = (pre(g), spec(g));
            assert_eq!(p.style, full.style);
            assert_eq!(names(&p), names(&full));
            assert!(p.notes[0].contains("preamble"), "{:?}", p.notes);
        }
        // a signature is exact either way
        assert_eq!(pre(golden!("signature")), spec(golden!("signature")));
    }
}
