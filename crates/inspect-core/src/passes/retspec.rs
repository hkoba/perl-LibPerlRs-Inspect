//! Return value / exception spec inference:
//!   - explicit `return` (line number, value shape, expressions)
//!   - the implicit final expression
//!   - exceptions via `die` / Carp::croak / Carp::confess
//!   - whether `wantarray` is used (a marker of context dependence)

use serde::{Deserialize, Serialize};

use crate::ir::{OpDetail, OpNode, SubIr};
use crate::render::render;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetSpec {
    pub returns: Vec<RetSite>,
    pub throws: Vec<ThrowSite>,
    pub uses_wantarray: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetSite {
    pub line: Option<u32>,
    /// empty | scalar | list | implicit
    pub kind: String,
    pub exprs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThrowSite {
    pub line: Option<u32>,
    /// die | croak | confess
    pub via: String,
    pub message: Option<String>,
}

pub fn analyze_returns(ir: &SubIr) -> RetSpec {
    let mut returns = Vec::new();
    let mut throws = Vec::new();
    let mut uses_wantarray = false;

    super::walk_with_lines(ir, |node, line| match node.name.as_str() {
        "return" => {
            let exprs = return_exprs(ir, node);
            returns.push(RetSite {
                line,
                kind: match exprs.len() {
                    0 => "empty",
                    1 => "scalar",
                    _ => "list",
                }
                .to_string(),
                exprs,
            });
        }
        "die" => {
            let exprs = return_exprs(ir, node);
            throws.push(ThrowSite {
                line,
                via: "die".into(),
                message: exprs.first().cloned(),
            });
        }
        "wantarray" => uses_wantarray = true,
        "entersub" => {
            if let Some((via, msg)) = croak_call(ir, node) {
                throws.push(ThrowSite {
                    line,
                    via,
                    message: msg,
                });
            }
        }
        _ => {}
    });

    // implicit final expression: if the last statement of the body lineseq is
    // not a return or a control structure, it becomes the return value
    if let Some((line, last)) = super::statements(ir).into_iter().last() {
        let l = last.skip_null();
        if !matches!(
            l.name.as_str(),
            "return" | "leaveloop" | "unstack" | "lineseq"
        ) {
            returns.push(RetSite {
                line,
                kind: "implicit".into(),
                exprs: vec![render(ir, last)],
            });
        }
    }

    RetSpec {
        returns,
        throws,
        uses_wantarray,
    }
}

/// Render the non-pushmark children of a return / die (LISTOP)
pub(crate) fn return_exprs(ir: &SubIr, n: &OpNode) -> Vec<String> {
    crate::render::kid_list(ir, n)
}

/// If the entersub is a croak/confess (Carp) call, return (via, message)
pub(crate) fn croak_call(ir: &SubIr, n: &OpNode) -> Option<(String, Option<String>)> {
    let mut via: Option<String> = None;
    let mut args: Vec<String> = Vec::new();

    fn scan(ir: &SubIr, n: &OpNode, via: &mut Option<String>, args: &mut Vec<String>) {
        for k in n.kids.iter().map(|k| k.skip_null()) {
            match k.name.as_str() {
                "pushmark" | "padrange" => {}
                "gv" => {
                    if let OpDetail::Gv { name, .. } = &k.detail {
                        if name == "croak" || name == "confess" {
                            *via = Some(name.clone());
                        }
                    }
                }
                "null" => scan(ir, k, via, args),
                "entersub" => {} // do not look inside nested calls
                _ => args.push(render(ir, k)),
            }
        }
    }
    scan(ir, n, &mut via, &mut args);
    via.map(|v| (v, args.first().cloned()))
}
