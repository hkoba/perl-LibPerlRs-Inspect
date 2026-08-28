//! lint 検出。v0 は take1 の Project.md にあるテーマから
//! `my $var = EXPR if COND;` (条件付き my) を実装する。
//!
//! この構文は条件不成立時に変数が前回呼び出しの値を保持しうる
//! 悪名高いパターン (perldoc perlsyn も「未定義動作」扱い)。
//! optree 上は「文のトップが and/or/dor で、その分岐側に
//! OPpLVAL_INTRO 付きの pad intro op が裸で現れる」形になる。
//! ブロック形 `if (COND) { my $x = ...; }` は分岐側が lineseq/scope
//! (中に COP) になるので、そこで枝刈りして誤検出を避ける。

use serde::{Deserialize, Serialize};

use crate::ir::{OpClass, OpNode, SubIr};

/// op_private の OPpLVAL_INTRO (pad 系 op と padrange で bit 7)。
/// IR は Perl バージョン固定なので定数をここに持つ
const OPP_LVAL_INTRO: u8 = 0x80;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LintsSpec {
    pub lints: Vec<Lint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lint {
    /// 安定 id (例: "my-in-conditional-statement")
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
        // kids[0] は条件。分岐側 (kids[1..]) に裸の pad intro を探す
        for branch in &top.kids[1..] {
            let mut hits = Vec::new();
            find_bare_intro(ir, branch, &mut hits);
            for var in hits {
                lints.push(Lint {
                    id: "my-in-conditional-statement".into(),
                    severity: "error".into(),
                    line,
                    message: format!(
                        "`my {} = EXPR {} COND` は条件不成立時に {} が\
                         前回の値を保持しうる (perlsyn で非推奨の構文)",
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

/// スコープ境界 (lineseq / scope / enter* / leave* / COP) を越えずに
/// 到達できる OPpLVAL_INTRO 付き pad intro op を集める
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
    // padrange は private の bit 7 が intro、targ が先頭 pad ix
    if n.name == "padrange" && n.private & OPP_LVAL_INTRO != 0 {
        if let Some(name) = ir.pad_name(n.targ) {
            out.push(name.to_string());
        }
    }
    for k in &n.kids {
        find_bare_intro(ir, k, out);
    }
}
