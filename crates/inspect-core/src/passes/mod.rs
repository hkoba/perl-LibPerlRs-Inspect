//! Analysis passes. All of them are pure Rust operating on SubIr (the owned IR).

pub mod argspec;
pub mod lints;
pub mod logic;
pub mod retspec;
pub mod types;

use serde::{Deserialize, Serialize};

use crate::ir::{OpNode, SubIr};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub args: argspec::ArgSpec,
    pub returns: retspec::RetSpec,
    pub logic: logic::LogicSpec,
    pub types: types::TypesSpec,
    pub lints: lints::LintsSpec,
}

pub fn analyze(ir: &SubIr) -> AnalysisReport {
    AnalysisReport {
        args: argspec::analyze_args(ir),
        returns: retspec::analyze_returns(ir),
        logic: logic::analyze_logic(ir),
        types: types::analyze_types(ir),
        lints: lints::analyze_lints(ir),
    }
}

/// Statement list of the sub body: split the children of leavesub → lineseq
/// at COPs into a sequence of (line number, statement op)
pub(crate) fn statements(ir: &SubIr) -> Vec<(Option<u32>, &OpNode)> {
    let root = ir.root.skip_null();
    let Some(body) = root.kids.first().map(|k| k.skip_null()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut line: Option<u32> = None;
    for k in &body.kids {
        if k.class == crate::ir::OpClass::Cop {
            if let crate::ir::OpDetail::Cop { line: l, .. } = &k.detail {
                line = Some(*l);
            }
            continue;
        }
        out.push((line, k));
    }
    out
}

/// Visit every node in pre-order while tracking the current COP line number
pub(crate) fn walk_with_lines<'a>(ir: &'a SubIr, mut f: impl FnMut(&'a OpNode, Option<u32>)) {
    fn rec<'a>(
        node: &'a OpNode,
        line: &mut Option<u32>,
        f: &mut impl FnMut(&'a OpNode, Option<u32>),
    ) {
        if let crate::ir::OpDetail::Cop { line: l, .. } = &node.detail {
            *line = Some(*l);
        }
        f(node, *line);
        for k in &node.kids {
            rec(k, line, f);
        }
    }
    let mut line = None;
    rec(&ir.root, &mut line, &mut f);
}
