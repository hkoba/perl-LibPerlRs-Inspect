//! 変数型推論 v0 — フロー非依存の制約 (facet) 収集。
//!
//! 各 lexical について「どう使われたか」を facet として集める:
//!   - ARRAYref / HASHref / SCALARref / CODEref: deref された
//!     (multideref の padsv base、rv2av/rv2hv/rv2sv over padsv、&$x 呼び出し)
//!   - Object: メソッド呼び出しのインボカント (メソッド名一覧も記録 —
//!     メソッド名 typo 検出の下地)
//!   - Num / Str: 数値演算・文字列演算のオペランド
//! PadnameTYPE (`my Foo $x`) は宣言型としてそのまま載せる。
//! 相反する deref facet (ARRAYref と HASHref 等) は conflict として報告。
//!
//! v0 の制限: フロー非依存 (分岐ごとの型は区別しない)、要素型は追わない、
//! multiconcat の aux 未デコード (kids に現れる padsv のみ拾う)。

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
    "add", "subtract", "multiply", "divide", "modulo", "pow", "preinc", "postinc", "predec",
    "postdec", "negate", "abs", "int", "i_add", "i_subtract", "i_multiply", "i_divide",
    "i_modulo", "i_negate", "lt", "gt", "le", "ge", "eq", "ne", "ncmp",
];

const STRING_OPS: &[&str] = &[
    "concat", "multiconcat", "slt", "sgt", "sle", "sge", "seq", "sne", "scmp", "lc", "uc",
    "lcfirst", "ucfirst",
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
        // multideref: padsv base = その lexical が ref として deref された
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
                                "{} として deref ({})",
                                if step.container == "array" {
                                    "配列リファレンス"
                                } else {
                                    "ハッシュリファレンス"
                                },
                                crate::render::render_mderef(ir, steps),
                            ),
                        );
                    }
                }
            }
        }

        match node.name.as_str() {
            // 非 multideref 形: @$x / %$x / $$x
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
                            format!("{} による deref", node.name),
                        );
                    }
                }
            }
            "entersub" => {
                // メソッド呼び出し: インボカントの lexical に Object facet
                if let Some((invocant_targ, method)) = method_call_on_pad(node) {
                    add(
                        &mut acc,
                        invocant_targ,
                        node.id,
                        line,
                        "Object",
                        format!("メソッド呼び出し ->{}", method),
                    );
                    let a = acc.entry(invocant_targ).or_default();
                    if !a.methods.contains(&method) {
                        a.methods.push(method);
                    }
                }
                // &$x / $x->(...) : rv2cv (null 化) 越しの padsv
                if let Some(targ) = coderef_callee_pad(node) {
                    add(
                        &mut acc,
                        targ,
                        node.id,
                        line,
                        "CODEref",
                        "コードリファレンスとして呼び出し".into(),
                    );
                }
            }
            name if NUMERIC_OPS.contains(&name) => {
                for k in node.kids.iter().map(|k| k.skip_null()) {
                    if k.name == "padsv" {
                        add(&mut acc, k.targ, node.id, line, "Num", format!("数値演算 {}", name));
                    }
                }
            }
            name if STRING_OPS.contains(&name) => {
                for k in node.kids.iter().map(|k| k.skip_null()) {
                    if k.name == "padsv" {
                        add(&mut acc, k.targ, node.id, line, "Str", format!("文字列演算 {}", name));
                    }
                }
            }
            _ => {}
        }
    });

    // pad テーブルと突き合わせて出力を組み立てる
    let mut vars = Vec::new();
    for entry in &ir.pad {
        let Some(name) = entry.name.clone() else {
            continue;
        };
        let a = acc.remove(&(entry.ix as u64)).unwrap_or_default();
        if a.facets.is_empty() && entry.typ.is_none() {
            continue; // 情報が無い変数は載せない
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

/// deref 系 facet は相互排他 — 複数あれば矛盾
fn deref_conflicts(facets: &[String]) -> Vec<String> {
    let derefs: Vec<&str> = facets
        .iter()
        .map(String::as_str)
        .filter(|f| matches!(*f, "ARRAYref" | "HASHref" | "SCALARref" | "CODEref"))
        .collect();
    if derefs.len() > 1 {
        vec![format!("{} として同時に使われている", derefs.join(" と "))]
    } else {
        Vec::new()
    }
}

/// entersub がメソッド呼び出しで、インボカントが padsv なら
/// (targ, メソッド名) を返す
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
                // 最初の値がインボカント。それが padsv 以外なら追わない
                *seen_first = true;
            }
        }
    }
}

/// entersub の呼び先が null(ex-rv2cv) 越しの padsv (= $cb->(...) / &$cb())
/// なら targ を返す
fn coderef_callee_pad(entersub: &OpNode) -> Option<u64> {
    // 呼び先は entersub の最後の kid (メソッド呼び出しでない場合)
    let callee = entersub.kids.last()?;
    if callee.op_type == 0 && callee.was.as_deref() == Some("rv2cv") {
        let k = callee.kids.first()?.skip_null();
        if k.name == "padsv" {
            return Some(k.targ);
        }
    }
    None
}
