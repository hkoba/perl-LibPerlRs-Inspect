//! Variable type inference v0 — flow-insensitive constraint (facet) collection.
//!
//! For each lexical, collect "how it was used" as facets:
//!   - ARRAYref / HASHref / SCALARref / CODEref: it was dereferenced
//!     (padsv base of a multideref, rv2av/rv2hv/rv2sv over padsv, &$x call)
//!   - Object: invocant of a method call (the method names are recorded too —
//!     groundwork for detecting method-name typos)
//!   - Num / Str: operand of a numeric / string operation
//! PadnameTYPE (`my Foo $x`) is reported as-is as the declared type.
//! Contradictory deref facets (ARRAYref and HASHref etc.) are reported as conflicts.
//!
//! v0 limitations: flow-insensitive (types are not distinguished per branch),
//! element types are not tracked, multiconcat aux is not decoded (only padsv
//! appearing among the kids is picked up).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ir::{OpDetail, OpNode, SubIr};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypesSpec {
    pub vars: Vec<VarTypes>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VarTypes {
    pub name: String,
    pub pad_ix: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub declared_type: Option<String>,
    pub facets: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub methods: Vec<String>,
    pub evidence: Vec<Evidence>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub op_id: u32,
    pub line: Option<u32>,
    pub facet: String,
    pub why: String,
}

const NUMERIC_OPS: &[&str] = &[
    "add",
    "subtract",
    "multiply",
    "divide",
    "modulo",
    "pow",
    "preinc",
    "postinc",
    "predec",
    "postdec",
    "negate",
    "abs",
    "int",
    "i_add",
    "i_subtract",
    "i_multiply",
    "i_divide",
    "i_modulo",
    "i_negate",
    "lt",
    "gt",
    "le",
    "ge",
    "eq",
    "ne",
    "ncmp",
];

const STRING_OPS: &[&str] = &[
    "concat",
    "multiconcat",
    "slt",
    "sgt",
    "sle",
    "sge",
    "seq",
    "sne",
    "scmp",
    "lc",
    "uc",
    "lcfirst",
    "ucfirst",
];

#[derive(Default)]
struct Acc {
    facets: Vec<String>,
    methods: Vec<String>,
    evidence: Vec<Evidence>,
}

fn add(
    acc: &mut BTreeMap<u64, Acc>,
    targ: u64,
    op_id: u32,
    line: Option<u32>,
    facet: &str,
    why: String,
) {
    let a = acc.entry(targ).or_default();
    if !a.facets.contains(&facet.to_string()) {
        a.facets.push(facet.to_string());
    }
    a.evidence.push(Evidence {
        op_id,
        line,
        facet: facet.into(),
        why,
    });
}

pub fn analyze_types(ir: &SubIr) -> TypesSpec {
    let mut acc: BTreeMap<u64, Acc> = BTreeMap::new();

    super::walk_with_lines(ir, |node, line| {
        // multideref: a padsv base means that lexical was dereferenced as a ref
        if let OpDetail::MultiDeref { steps } = &node.detail {
            for step in steps {
                if step.base == "padsv" {
                    if let Some(targ) = step.base_targ {
                        let facet = if step.container == "array" {
                            "ARRAYref"
                        } else {
                            "HASHref"
                        };
                        add(
                            &mut acc,
                            targ,
                            node.id,
                            line,
                            facet,
                            format!(
                                "dereferenced as {} ({})",
                                if step.container == "array" {
                                    "array ref"
                                } else {
                                    "hash ref"
                                },
                                crate::render::render_mderef(ir, steps, None),
                            ),
                        );
                    }
                }
            }
        }

        match node.name.as_str() {
            // non-multideref forms: @$x / %$x / $$x
            "rv2av" | "rv2hv" | "rv2sv" => {
                if let Some(k) = node.kids.first().map(|k| k.skip_null()) {
                    if k.name == "padsv" {
                        let facet = match node.name.as_str() {
                            "rv2av" => "ARRAYref",
                            "rv2hv" => "HASHref",
                            _ => "SCALARref",
                        };
                        add(
                            &mut acc,
                            k.targ,
                            node.id,
                            line,
                            facet,
                            format!("dereferenced via {}", node.name),
                        );
                    }
                }
            }
            "entersub" => {
                // method call: Object facet on the invocant lexical
                if let Some((invocant_targ, method)) = method_call_on_pad(node) {
                    add(
                        &mut acc,
                        invocant_targ,
                        node.id,
                        line,
                        "Object",
                        format!("method call ->{}", method),
                    );
                    let a = acc.entry(invocant_targ).or_default();
                    if !a.methods.contains(&method) {
                        a.methods.push(method);
                    }
                }
                // &$x / $x->(...) : padsv through a (nulled) rv2cv
                if let Some(targ) = coderef_callee_pad(node) {
                    add(
                        &mut acc,
                        targ,
                        node.id,
                        line,
                        "CODEref",
                        "called as a code ref".into(),
                    );
                }
            }
            name if NUMERIC_OPS.contains(&name) => {
                for k in node.kids.iter().map(|k| k.skip_null()) {
                    if k.name == "padsv" {
                        add(
                            &mut acc,
                            k.targ,
                            node.id,
                            line,
                            "Num",
                            format!("numeric op {}", name),
                        );
                    }
                }
            }
            name if STRING_OPS.contains(&name) => {
                for k in node.kids.iter().map(|k| k.skip_null()) {
                    if k.name == "padsv" {
                        add(
                            &mut acc,
                            k.targ,
                            node.id,
                            line,
                            "Str",
                            format!("string op {}", name),
                        );
                    }
                }
            }
            _ => {}
        }
    });

    // assemble the output by matching against the pad table
    let mut vars = Vec::new();
    for entry in &ir.pad {
        let Some(name) = entry.name.clone() else {
            continue;
        };
        let a = acc.remove(&(entry.ix as u64)).unwrap_or_default();
        if a.facets.is_empty() && entry.typ.is_none() {
            continue; // omit variables with no information
        }
        let conflicts = deref_conflicts(&a.facets);
        vars.push(VarTypes {
            name,
            pad_ix: entry.ix,
            declared_type: entry.typ.clone(),
            facets: a.facets,
            methods: a.methods,
            evidence: a.evidence,
            conflicts,
        });
    }
    TypesSpec { vars }
}

/// Deref facets are mutually exclusive — more than one is a conflict
fn deref_conflicts(facets: &[String]) -> Vec<String> {
    let derefs: Vec<&str> = facets
        .iter()
        .map(String::as_str)
        .filter(|f| matches!(*f, "ARRAYref" | "HASHref" | "SCALARref" | "CODEref"))
        .collect();
    if derefs.len() > 1 {
        vec![format!("used both as {}", derefs.join(" and "))]
    } else {
        Vec::new()
    }
}

/// If the entersub is a method call and the invocant is a padsv,
/// return (targ, method name)
fn method_call_on_pad(entersub: &OpNode) -> Option<(u64, String)> {
    let mut invocant: Option<u64> = None;
    let mut method: Option<String> = None;
    scan_call(entersub, &mut invocant, &mut method, &mut false);
    Some((invocant?, method?))
}

fn scan_call(
    n: &OpNode,
    invocant: &mut Option<u64>,
    method: &mut Option<String>,
    seen_first: &mut bool,
) {
    for k in n.kids.iter().map(|k| k.skip_null()) {
        match k.name.as_str() {
            "pushmark" => {}
            "null" | "list" => scan_call(k, invocant, method, seen_first),
            "method_named" | "method" => {
                if let OpDetail::Method { name: Some(m) } = &k.detail {
                    *method = Some(m.clone());
                }
            }
            "padsv" if !*seen_first => {
                *seen_first = true;
                *invocant = Some(k.targ);
            }
            _ => {
                // the first value is the invocant. If it is not a padsv, do not track it
                *seen_first = true;
            }
        }
    }
}

/// If the entersub's callee is a padsv through null(ex-rv2cv) (= $cb->(...) / &$cb()),
/// return its targ
fn coderef_callee_pad(entersub: &OpNode) -> Option<u64> {
    // the callee is the entersub's last kid (when not a method call)
    let callee = entersub.kids.last()?;
    if callee.op_type == 0 && callee.was.as_deref() == Some("rv2cv") {
        let k = callee.kids.first()?.skip_null();
        if k.name == "padsv" {
            return Some(k.targ);
        }
    }
    None
}
