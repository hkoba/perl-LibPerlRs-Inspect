//! Argument spec inference. Four-tier pattern detection (in priority order):
//!   1. signature (argcheck/argelem, or multiparam on perl 5.44+) — yields
//!      the exact arity
//!   2. `my (...) = @_` (aassign + rv2av *_)
//!   3. a run of `my $x = shift` (padsv_store + shift)
//!   4. direct `$_[n]` access — only a lower bound on arity

use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use crate::ir::{NamedParam, OpDetail, OpNode, SubIr};
use crate::render::render;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgSpec {
    /// signature | unpack | shift | positional | mixed | none
    pub style: String,
    pub min_arity: u64,
    /// None = no upper bound (slurpy or unchecked)
    pub max_arity: Option<u64>,
    pub params: Vec<Param>,
    pub invocant_guess: Option<String>,
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
}

pub fn analyze_args(ir: &SubIr) -> ArgSpec {
    if let Some(spec) = detect_signature(ir) {
        return spec;
    }
    detect_classic(ir)
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
    }
}

/// Classic patterns: pick up the run of shifts / my(...)=@_ from the start of
/// the body, and $_[n] from the whole tree
fn detect_classic(ir: &SubIr) -> ArgSpec {
    let mut params: Vec<Param> = Vec::new();
    let mut styles: Vec<&str> = Vec::new();
    let mut slurpy = false;
    let mut next_index: u64 = 0;

    // scan the preamble (the leading run of argument-extraction statements)
    for (_line, stmt) in super::statements(ir) {
        let s = stmt.skip_null();
        if let Some(name) = shift_into_pad(ir, s) {
            params.push(Param {
                name: Some(name),
                index: next_index,
                default: None,
                source: "shift".into(),
                key: None,
            });
            next_index += 1;
            if !styles.contains(&"shift") {
                styles.push("shift");
            }
            continue;
        }
        if let Some((names, has_slurpy)) = unpack_from_args(ir, s) {
            for name in names {
                let is_slurpy_param =
                    name.starts_with('@') || name.starts_with('%');
                params.push(Param {
                    name: Some(name),
                    index: next_index,
                    default: None,
                    source: "unpack".into(),
                    key: None,
                });
                if !is_slurpy_param {
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
    let mut max_elem_index: Option<i64> = None;
    super::walk_with_lines(ir, |node, _| {
        if let Some(ix) = arg_elem_index(node) {
            if max_elem_index.is_none_or(|m| ix > m) {
                max_elem_index = Some(ix);
            }
        }
    });
    if let Some(m) = max_elem_index {
        if m >= 0 {
            if !styles.contains(&"positional") {
                styles.push("positional");
            }
            for ix in 0..=m {
                let ixu = ix as u64;
                if !params.iter().any(|p| p.index == ixu) {
                    params.push(Param {
                        name: None,
                        index: ixu,
                        default: None,
                        source: "elem".into(),
                        key: None,
                    });
                }
            }
        }
    }

    let style = match styles.len() {
        0 => "none".to_string(),
        1 => styles[0].to_string(),
        _ => "mixed".to_string(),
    };
    let positional = params
        .iter()
        .filter(|p| {
            p.name
                .as_deref()
                .is_none_or(|n| !n.starts_with('@') && !n.starts_with('%'))
        })
        .count() as u64;
    // Classic patterns cannot distinguish required from optional. Only
    // unconditional $_[n] access gives grounds for a lower bound
    let min_arity = max_elem_index.map_or(0, |m| (m + 1).max(0) as u64);
    ArgSpec {
        style,
        min_arity,
        // an effective upper bound is only known for unpack without a slurpy
        max_arity: if !slurpy && styles == ["unpack"] {
            Some(positional)
        } else {
            None
        },
        invocant_guess: guess_invocant(&params),
        params,
    }
}

/// `my $x = shift;` → padsv_store(targ=$x) with a bare shift as its child
/// (the older form sassign(shift, padsv) is also handled)
fn shift_into_pad(ir: &SubIr, s: &OpNode) -> Option<String> {
    match s.name.as_str() {
        "padsv_store" => {
            let k = s.kids.first()?.skip_null();
            if k.name == "shift" && shifts_default_args(k) {
                ir.pad_name(s.targ).map(String::from)
            } else {
                None
            }
        }
        "sassign" => {
            let rhs = s.kids.first()?.skip_null();
            let lhs = s.kids.get(1)?.skip_null();
            if rhs.name == "shift" && shifts_default_args(rhs) && lhs.name == "padsv" {
                ir.pad_name(lhs.targ).map(String::from)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether the shift operates on @_ (bare = @_; if explicit, check for rv2av(gv *_))
fn shifts_default_args(shift: &OpNode) -> bool {
    if shift.kids.is_empty() {
        return true; // a bare shift inside a sub is on @_
    }
    subtree_has_underscore_av(shift)
}

/// `my (...) = @_;` → aassign(rv2av(*_) on the RHS, a run of padsv/padav/padhv on the LHS)
fn unpack_from_args(ir: &SubIr, s: &OpNode) -> Option<(Vec<String>, bool)> {
    if s.name != "aassign" {
        return None;
    }
    let rhs = s.kids.first()?;
    let lhs = s.kids.get(1)?;
    if !subtree_has_underscore_av(rhs) {
        return None;
    }
    let mut names = Vec::new();
    let mut slurpy = false;
    collect_lhs_targets(ir, lhs, &mut names, &mut slurpy);
    if names.is_empty() {
        return None;
    }
    Some((names, slurpy))
}

fn collect_lhs_targets(ir: &SubIr, n: &OpNode, names: &mut Vec<String>, slurpy: &mut bool) {
    let n = n.skip_null();
    match n.name.as_str() {
        "padsv" => {
            if let Some(name) = ir.pad_name(n.targ) {
                names.push(name.to_string());
            }
        }
        "padav" | "padhv" => {
            if let Some(name) = ir.pad_name(n.targ) {
                names.push(name.to_string());
                *slurpy = true;
            }
        }
        _ => {
            for k in &n.kids {
                collect_lhs_targets(ir, k, names, slurpy);
            }
        }
    }
}

/// Whether the subtree contains @_ (gv *_ under rv2av / via padrange)
fn subtree_has_underscore_av(n: &OpNode) -> bool {
    let n = n.skip_null();
    if n.name == "rv2av" {
        if let Some(k) = n.kids.first().map(|k| k.skip_null()) {
            if let OpDetail::Gv { name, stash } = &k.detail {
                if name == "_" && stash.as_deref().unwrap_or("main") == "main" {
                    return true;
                }
            }
        }
    }
    n.kids.iter().any(subtree_has_underscore_av)
}

/// `$_[n]`: the index is the op_private of aelemfast (PADOP, gv *_).
/// The aelem(rv2av(*_), const n) form is also covered
fn arg_elem_index(node: &OpNode) -> Option<i64> {
    let n = node; // do not skip nulls (the aelemfast under ex-aelem is the real op)
    if n.name == "aelemfast" {
        if let OpDetail::Gv { name, stash } = &n.detail {
            if name == "_" && stash.as_deref().unwrap_or("main") == "main" {
                return Some(n.private as i8 as i64);
            }
        }
    }
    if n.name == "aelem" && n.kids.len() == 2 && subtree_has_underscore_av(&n.kids[0]) {
        if let OpDetail::Const(crate::ir::SvLit::Iv(ix)) = &n.kids[1].skip_null().detail {
            return Some(*ix);
        }
    }
    None
}

fn guess_invocant(params: &[Param]) -> Option<String> {
    let first = params.iter().find(|p| p.index == 0)?;
    let name = first.name.as_deref()?;
    matches!(name, "$self" | "$class" | "$proto" | "$pkg").then(|| name.to_string())
}
