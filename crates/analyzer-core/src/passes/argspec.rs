//! 引数仕様の推定。4 段のパターン検出 (優先順):
//!   1. signature (argcheck/argelem) — 正確な arity が取れる
//!   2. `my (...) = @_` (aassign + rv2av *_)
//!   3. `my $x = shift` の列 (padsv_store + shift)
//!   4. `$_[n]` 直接アクセス — arity の下限のみ

use serde::{Deserialize, Serialize};

use crate::ir::{OpDetail, OpNode, SubIr};
use crate::render::render;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgSpec {
    /// signature | unpack | shift | positional | mixed | none
    pub style: String,
    pub min_arity: u64,
    /// None = 上限なし (slurpy or 未検査)
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
}

pub fn analyze_args(ir: &SubIr) -> ArgSpec {
    if let Some(spec) = detect_signature(ir) {
        return spec;
    }
    detect_classic(ir)
}

/// signature: null(ex-argcheck) 配下の argcheck / argelem / argdefelem
fn detect_signature(ir: &SubIr) -> Option<ArgSpec> {
    let mut check: Option<(u64, u64, Option<char>)> = None;
    let mut params: Vec<Param> = Vec::new();

    super::walk_with_lines(ir, |node, _line| match &node.detail {
        OpDetail::ArgCheck {
            params: p,
            opt,
            slurpy,
        } => {
            check = Some((*p, *opt, *slurpy));
        }
        OpDetail::ArgElem { index } => {
            // デフォルト式は argelem の子 argdefelem の子
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
            });
        }
        _ => {}
    });

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

/// 古典的パターン: 文頭から順に shift 列 / my(...)=@_ を拾い、
/// 全体から $_[n] を拾う
fn detect_classic(ir: &SubIr) -> ArgSpec {
    let mut params: Vec<Param> = Vec::new();
    let mut styles: Vec<&str> = Vec::new();
    let mut slurpy = false;
    let mut next_index: u64 = 0;

    // プリアンブル (先頭からの連続した引数取り出し文) を走査
    for (_line, stmt) in super::statements(ir) {
        let s = stmt.skip_null();
        if let Some(name) = shift_into_pad(ir, s) {
            params.push(Param {
                name: Some(name),
                index: next_index,
                default: None,
                source: "shift".into(),
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
        break; // 引数取り出しでない文が来たらプリアンブル終了
    }

    // $_[n] 直接アクセス (ツリー全体)
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
    // 古典的パターンでは必須/任意の区別は付かない。$_[n] の
    // 無条件アクセスだけは下限の根拠になる
    let min_arity = max_elem_index.map_or(0, |m| (m + 1).max(0) as u64);
    ArgSpec {
        style,
        min_arity,
        // unpack で slurpy が無い場合のみ実質上限が分かる
        max_arity: if !slurpy && styles == ["unpack"] {
            Some(positional)
        } else {
            None
        },
        invocant_guess: guess_invocant(&params),
        params,
    }
}

/// `my $x = shift;` → padsv_store(targ=$x) の子に裸の shift
/// (旧形式 sassign(shift, padsv) にも対応)
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

/// shift の対象が @_ か (裸 = @_、明示なら rv2av(gv *_) を確認)
fn shifts_default_args(shift: &OpNode) -> bool {
    if shift.kids.is_empty() {
        return true; // sub 内の裸の shift は @_
    }
    subtree_has_underscore_av(shift)
}

/// `my (...) = @_;` → aassign(RHS に rv2av(*_), LHS に padsv/padav/padhv 列)
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

/// サブツリーに @_ (rv2av 配下の gv *_ / padrange 経由) が含まれるか
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

/// `$_[n]`: aelemfast (PADOP, gv *_) の op_private が添字。
/// aelem(rv2av(*_), const n) 形もカバー
fn arg_elem_index(node: &OpNode) -> Option<i64> {
    let n = node; // null 透過しない (ex-aelem の下の aelemfast は実 op)
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
