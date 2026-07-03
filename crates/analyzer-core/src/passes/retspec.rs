//! 戻り値・例外仕様の推定:
//!   - 明示的な `return` (行番号・値の形状・式)
//!   - 暗黙の最終式
//!   - `die` / Carp::croak / Carp::confess による例外
//!   - `wantarray` 使用の有無 (コンテキスト依存の目印)

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

    // 暗黙の最終式: 本体 lineseq の最後の文が return や制御構造で
    // なければ、それが戻り値になる
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

/// return / die (LISTOP) の pushmark 以外の子を描画
pub(crate) fn return_exprs(ir: &SubIr, n: &OpNode) -> Vec<String> {
    n.kids
        .iter()
        .map(|k| k.skip_null())
        .filter(|k| k.name != "pushmark")
        .map(|k| render(ir, k))
        .collect()
}

/// entersub が croak/confess (Carp) 呼び出しなら (via, message) を返す
pub(crate) fn croak_call(ir: &SubIr, n: &OpNode) -> Option<(String, Option<String>)> {
    let mut via: Option<String> = None;
    let mut args: Vec<String> = Vec::new();

    fn scan(ir: &SubIr, n: &OpNode, via: &mut Option<String>, args: &mut Vec<String>) {
        for k in n.kids.iter().map(|k| k.skip_null()) {
            match k.name.as_str() {
                "pushmark" => {}
                "gv" => {
                    if let OpDetail::Gv { name, .. } = &k.detail {
                        if name == "croak" || name == "confess" {
                            *via = Some(name.clone());
                        }
                    }
                }
                "null" => scan(ir, k, via, args),
                "entersub" => {} // ネストした呼び出しの中までは見ない
                _ => args.push(render(ir, k)),
            }
        }
    }
    scan(ir, n, &mut via, &mut args);
    via.map(|v| (v, args.first().cloned()))
}
