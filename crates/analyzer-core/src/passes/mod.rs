//! 解析パス群。すべて SubIr (所有型 IR) 上で動く純 Rust コード。

pub mod argspec;
pub mod retspec;

use serde::{Deserialize, Serialize};

use crate::ir::{OpNode, SubIr};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub args: argspec::ArgSpec,
    pub returns: retspec::RetSpec,
}

pub fn analyze(ir: &SubIr) -> AnalysisReport {
    AnalysisReport {
        args: argspec::analyze_args(ir),
        returns: retspec::analyze_returns(ir),
    }
}

/// sub 本体の文リスト: leavesub → lineseq の子を COP で区切り、
/// (行番号, 文の op) の列にする
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

/// pre-order 走査で COP の行番号を追跡しながら各ノードを訪問する
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
