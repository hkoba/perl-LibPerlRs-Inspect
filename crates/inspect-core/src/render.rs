//! ミニ deparser — 小さな式サブツリーを Perl 風の文字列に描画する。
//!
//! B::Deparse の完全再実装ではない。デフォルト値・return 式・
//! ガード条件の原子式など「人に見せる短い式」を対象にした部分集合。
//! op を追加するときは /usr/share/perl5/B/Deparse.pm の該当 pp_* を
//! 参照すること。未対応 op は `<opname>` にフォールバックする。

use crate::ir::{OpDetail, OpNode, SubIr, SvLit};

pub fn render(ir: &SubIr, node: &OpNode) -> String {
    let n = node.skip_null();
    match n.name.as_str() {
        "padsv" => pad_var(ir, n.targ),
        "padsv_store" => format!(
            "my {} = {}",
            pad_var(ir, n.targ),
            n.kids.first().map(|k| render(ir, k)).unwrap_or_default()
        ),
        "padav" | "padhv" => pad_var(ir, n.targ),
        "const" => match &n.detail {
            OpDetail::Const(lit) => lit_str(lit),
            _ => "<const>".into(),
        },
        "gvsv" => match &n.detail {
            OpDetail::Gv { name, stash } => sigil_gv('$', name, stash.as_deref()),
            _ => "<gvsv>".into(),
        },
        "gv" => match &n.detail {
            OpDetail::Gv { name, stash } => sigil_gv('*', name, stash.as_deref()),
            _ => "<gv>".into(),
        },
        "rv2av" => prefix_deref('@', ir, n),
        "rv2hv" => prefix_deref('%', ir, n),
        "rv2sv" => prefix_deref('$', ir, n),
        "aelemfast" => match &n.detail {
            // 添字は op_private に埋め込まれる
            OpDetail::Gv { name, stash } => {
                let base = sigil_gv('$', name, stash.as_deref());
                format!("{}[{}]", base, n.private as i8)
            }
            _ => "<aelemfast>".into(),
        },
        "aelemfast_lex" => format!("{}[{}]", pad_var(ir, n.targ), n.private as i8),
        "aelem" => binop_like(ir, n, |a, i| {
            format!("{}[{}]", a.replacen('@', "$", 1), i)
        }),
        "helem" => binop_like(ir, n, |h, k| {
            format!("{}{{{}}}", h.replacen('%', "$", 1), k)
        }),
        "add" => infix(ir, n, "+"),
        "subtract" => infix(ir, n, "-"),
        "multiply" => infix(ir, n, "*"),
        "divide" => infix(ir, n, "/"),
        "modulo" => infix(ir, n, "%"),
        "pow" => infix(ir, n, "**"),
        "concat" => infix(ir, n, "."),
        "lt" => infix(ir, n, "<"),
        "gt" => infix(ir, n, ">"),
        "le" => infix(ir, n, "<="),
        "ge" => infix(ir, n, ">="),
        "eq" => infix(ir, n, "=="),
        "ne" => infix(ir, n, "!="),
        "ncmp" => infix(ir, n, "<=>"),
        "slt" => infix(ir, n, "lt"),
        "sgt" => infix(ir, n, "gt"),
        "sle" => infix(ir, n, "le"),
        "sge" => infix(ir, n, "ge"),
        "seq" => infix(ir, n, "eq"),
        "sne" => infix(ir, n, "ne"),
        "scmp" => infix(ir, n, "cmp"),
        "and" => infix(ir, n, "&&"),
        "or" => infix(ir, n, "||"),
        "dor" => infix(ir, n, "//"),
        "xor" => infix(ir, n, "xor"),
        "not" => format!("!{}", kid_str(ir, n, 0)),
        "negate" => format!("-{}", kid_str(ir, n, 0)),
        "defined" => format!("defined({})", kid_str(ir, n, 0)),
        "cond_expr" => {
            let c = kid_str(ir, n, 0);
            let t = kid_str(ir, n, 1);
            let f = kid_str(ir, n, 2);
            format!("{} ? {} : {}", c, t, f)
        }
        "sassign" => format!("{} = {}", kid_str(ir, n, 1), kid_str(ir, n, 0)),
        "shift" | "pop" => {
            if n.kids.is_empty() {
                // sub 内の裸の shift/pop は @_ が対象
                format!("{}(@_)", n.name)
            } else {
                format!("{}({})", n.name, kid_str(ir, n, 0))
            }
        }
        "wantarray" => "wantarray".into(),
        "entersub" => render_call(ir, n),
        "method_named" => match &n.detail {
            OpDetail::Method { name: Some(m) } => m.clone(),
            _ => "<method>".into(),
        },
        "multideref" => match &n.detail {
            OpDetail::MultiDeref { steps } => render_mderef(ir, steps),
            _ => "<multideref>".into(),
        },
        "stringify" => format!("\"{}\"", kid_list(ir, n).join("")),
        "list" | "pushmark" => kid_list(ir, n).join(", "),
        "undef" if n.kids.is_empty() => "undef".into(),
        "argdefelem" => kid_str(ir, n, 0),
        _ => format!("<{}>", n.name),
    }
}

fn infix(ir: &SubIr, n: &OpNode, op: &str) -> String {
    format!("{} {} {}", kid_str(ir, n, 0), op, kid_str(ir, n, 1))
}

fn binop_like(ir: &SubIr, n: &OpNode, f: impl Fn(String, String) -> String) -> String {
    f(kid_str(ir, n, 0), kid_str(ir, n, 1))
}

fn pad_var(ir: &SubIr, targ: u64) -> String {
    ir.pad_name(targ)
        .map(String::from)
        .unwrap_or_else(|| format!("$pad{}", targ))
}

fn lit_str(lit: &SvLit) -> String {
    match lit {
        SvLit::Undef => "undef".into(),
        SvLit::Iv(i) => i.to_string(),
        SvLit::Uv(u) => u.to_string(),
        SvLit::Nv(n) => n.to_string(),
        SvLit::Pv(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        SvLit::RefTo(inner) => format!("\\{}", lit_str(inner)),
        SvLit::Code => "sub {...}".into(),
        SvLit::Glob { name, stash } => sigil_gv('*', name, stash.as_deref()),
        SvLit::Other(s) => format!("<{}>", s),
    }
}

fn sigil_gv(sigil: char, name: &str, stash: Option<&str>) -> String {
    match stash {
        Some("main") | None => format!("{}{}", sigil, name),
        Some(pkg) => format!("{}{}::{}", sigil, pkg, name),
    }
}

fn prefix_deref(sigil: char, ir: &SubIr, n: &OpNode) -> String {
    match n.kids.first().map(|k| k.skip_null()) {
        Some(k) if k.name == "gv" => match &k.detail {
            OpDetail::Gv { name, stash } => sigil_gv(sigil, name, stash.as_deref()),
            _ => format!("<{}deref>", sigil),
        },
        Some(k) => format!("{}{{{}}}", sigil, render(ir, k)),
        None => format!("<{}deref>", sigil),
    }
}

fn kid_str(ir: &SubIr, n: &OpNode, i: usize) -> String {
    n.kids
        .get(i)
        .map(|k| render(ir, k))
        .unwrap_or_else(|| "<?>".into())
}

/// pushmark を除いた子の描画リスト
fn kid_list(ir: &SubIr, n: &OpNode) -> Vec<String> {
    n.kids
        .iter()
        .map(|k| k.skip_null())
        .filter(|k| k.name != "pushmark")
        .map(|k| render(ir, k))
        .collect()
}

/// multideref チェーンの描画: `$x->[0]{k}` / `$arr[0]` / `$h{k}` 等
pub(crate) fn render_mderef(ir: &SubIr, steps: &[crate::ir::DerefStep]) -> String {
    let mut out = String::new();
    for (i, s) in steps.iter().enumerate() {
        let subscript = |k: &Option<String>| -> String {
            let key = k.as_deref().unwrap_or("?");
            if s.container == "array" {
                format!("[{}]", key)
            } else {
                format!("{{{}}}", key)
            }
        };
        match s.base.as_str() {
            "padsv" => {
                // $x->[...] : ref を持つ lexical の deref
                let name = s
                    .base_targ
                    .and_then(|t| ir.pad_name(t).map(String::from))
                    .unwrap_or_else(|| "$?".into());
                out.push_str(&format!("{}->{}", name, subscript(&s.key)));
            }
            "padav" | "padhv" => {
                // $arr[...] / $h{...} : 集合 lexical の要素直接アクセス
                let name = s
                    .base_targ
                    .and_then(|t| ir.pad_name(t).map(String::from))
                    .unwrap_or_else(|| "?".into());
                out.push_str(&format!("${}{}", &name[1..], subscript(&s.key)));
            }
            "gvsv" => out.push_str(&format!(
                "${}->{}",
                s.base_name.as_deref().unwrap_or("?"),
                subscript(&s.key)
            )),
            "gvav" | "gvhv" => out.push_str(&format!(
                "${}{}",
                s.base_name.as_deref().unwrap_or("?"),
                subscript(&s.key)
            )),
            // chain: 直前ステップの結果への添字 (arrow 省略記法)
            "chain" if i > 0 => out.push_str(&subscript(&s.key)),
            _ => out.push_str(&format!("<expr>{}", subscript(&s.key))),
        }
    }
    if out.is_empty() {
        "<multideref>".into()
    } else {
        out
    }
}

/// entersub の描画: `f(args)` / `$obj->meth(args)`
fn render_call(ir: &SubIr, n: &OpNode) -> String {
    // entersub の子 (null 透過) から GV (呼び先) とメソッド op を探す
    let mut args: Vec<String> = Vec::new();
    let mut callee: Option<String> = None;
    let mut method: Option<String> = None;

    fn scan(
        ir: &SubIr,
        n: &OpNode,
        args: &mut Vec<String>,
        callee: &mut Option<String>,
        method: &mut Option<String>,
    ) {
        for k in n.kids.iter().map(|k| k.skip_null()) {
            match k.name.as_str() {
                "pushmark" => {}
                "null" => scan(ir, k, args, callee, method),
                "gv" => {
                    if let OpDetail::Gv { name, stash } = &k.detail {
                        *callee = Some(match stash.as_deref() {
                            Some("main") | None => name.clone(),
                            Some(pkg) => format!("{}::{}", pkg, name),
                        });
                    }
                }
                "method_named" | "method" => {
                    *method = Some(match &k.detail {
                        OpDetail::Method { name: Some(m) } => m.clone(),
                        _ => "<dynamic>".into(),
                    });
                }
                _ => args.push(render(ir, k)),
            }
        }
    }
    scan(ir, n, &mut args, &mut callee, &mut method);

    if let Some(m) = method {
        let invocant = if args.is_empty() {
            "<?>".to_string()
        } else {
            args.remove(0)
        };
        format!("{}->{}({})", invocant, m, args.join(", "))
    } else {
        format!(
            "{}({})",
            callee.unwrap_or_else(|| "<code>".into()),
            args.join(", ")
        )
    }
}
