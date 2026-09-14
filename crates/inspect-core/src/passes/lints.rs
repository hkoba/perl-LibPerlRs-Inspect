//! Lint detection. v0 implements `my $var = EXPR if COND;` (conditional my),
//! one of the themes from take1's Project.md.
//!
//! This construct is a notorious pattern where, when the condition is false,
//! the variable may retain its value from the previous call (perldoc perlsyn
//! also treats it as "undefined behavior"). In the optree it appears as
//! "the statement's top op is and/or/dor, and a pad intro op with
//! OPpLVAL_INTRO appears bare on the branch side". The block form
//! `if (COND) { my $x = ...; }` has a lineseq/scope (containing a COP) on the
//! branch side, so we prune there to avoid false positives.

use serde::{Deserialize, Serialize};

use crate::ir::{OpClass, OpNode, SubIr};

/// OPpLVAL_INTRO in op_private (bit 7 for pad ops and padrange).
/// The IR is pinned to a Perl version, so the constant lives here
const OPP_LVAL_INTRO: u8 = 0x80;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LintsSpec {
    pub lints: Vec<Lint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lint {
    /// Stable id (e.g. "my-in-conditional-statement")
    pub id: String,
    /// error | warning
    pub severity: String,
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub var: Option<String>,
    pub message: String,
}

pub fn analyze_lints(ir: &SubIr) -> LintsSpec {
    let mut lints = Vec::new();

    for (line, stmt) in super::statements(ir) {
        let top = stmt.skip_null();
        if !matches!(top.name.as_str(), "and" | "or" | "dor") || top.kids.len() < 2 {
            continue;
        }
        // kids[0] is the condition. Look for a bare pad intro on the branch side (kids[1..])
        for branch in &top.kids[1..] {
            let mut hits = Vec::new();
            find_bare_intro(ir, branch, &mut hits);
            for var in hits {
                lints.push(Lint {
                    id: "my-in-conditional-statement".into(),
                    severity: "error".into(),
                    line,
                    message: format!(
                        "`my {} = EXPR {} COND` may leave {} holding its previous value \
                         when COND is false (deprecated per perlsyn)",
                        var,
                        if top.name == "or" { "unless" } else { "if" },
                        var,
                    ),
                    var: Some(var),
                });
            }
        }
    }

    LintsSpec { lints }
}

/// Collect pad intro ops with OPpLVAL_INTRO that are reachable without
/// crossing a scope boundary (lineseq / scope / enter* / leave* / COP)
fn find_bare_intro(ir: &SubIr, n: &OpNode, out: &mut Vec<String>) {
    if n.class == OpClass::Cop
        || n.name == "lineseq"
        || n.name == "scope"
        || n.name.starts_with("enter")
        || n.name.starts_with("leave")
    {
        return;
    }
    if matches!(
        n.name.as_str(),
        "padsv_store" | "padsv" | "padav" | "padhv"
    ) && n.private & OPP_LVAL_INTRO != 0
    {
        if let Some(name) = ir.pad_name(n.targ) {
            out.push(name.to_string());
        }
    }
    // for padrange, bit 7 of private is intro and targ is the first pad ix
    if n.name == "padrange" && n.private & OPP_LVAL_INTRO != 0 {
        if let Some(name) = ir.pad_name(n.targ) {
            out.push(name.to_string());
        }
    }
    for k in &n.kids {
        find_bare_intro(ir, k, out);
    }
}
