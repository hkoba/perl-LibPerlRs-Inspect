//! SubIr の人間可読ダンプ (デバッグ用)。B::Concise の雰囲気に寄せた
//! ツリー表示。純 Rust なのでインタプリタ無しでテストできる。

use crate::ir::{OpDetail, OpNode, SubIr};

pub fn dump(ir: &SubIr) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "sub file={} lines={} start_id={}\n",
        ir.file.as_deref().unwrap_or("?"),
        match ir.lines {
            Some((a, b)) => format!("{}-{}", a, b),
            None => "?".to_string(),
        },
        match ir.start_id {
            Some(id) => id.to_string(),
            None => "?".to_string(),
        },
    ));
    if !ir.pad.is_empty() {
        out.push_str("pad:");
        for e in &ir.pad {
            out.push_str(&format!(
                " {}:{}{}",
                e.ix,
                e.name.as_deref().unwrap_or("?"),
                match &e.typ {
                    Some(t) => format!("({})", t),
                    None => String::new(),
                }
            ));
        }
        out.push('\n');
    }
    rec(&ir.root, 0, ir, &mut out);
    out
}

fn rec(node: &OpNode, depth: usize, ir: &SubIr, out: &mut String) {
    out.push_str(&"  ".repeat(depth));
    out.push_str(&format!("{}: {}", node.id, node.name));
    if let Some(was) = &node.was {
        out.push_str(&format!(" (ex-{})", was));
    }
    out.push_str(&format!(" [{}]", node.class));
    if node.targ != 0 {
        match ir.pad_name(node.targ) {
            Some(n) => out.push_str(&format!(" targ={}({})", node.targ, n)),
            None => out.push_str(&format!(" targ={}", node.targ)),
        }
    }
    if let Some(n) = node.next {
        out.push_str(&format!(" next->{}", n));
    }
    if let Some(o) = node.other {
        out.push_str(&format!(" other->{}", o));
    }
    match &node.detail {
        OpDetail::None => {}
        OpDetail::Const(lit) => out.push_str(&format!(" const={:?}", lit)),
        OpDetail::Gv { name, stash } => out.push_str(&format!(
            " gv=*{}::{}",
            stash.as_deref().unwrap_or("?"),
            name
        )),
        OpDetail::Cop { file: _, line } => out.push_str(&format!(" line={}", line)),
        OpDetail::Method { name } => out.push_str(&format!(
            " method={}",
            name.as_deref().unwrap_or("<dynamic>")
        )),
        OpDetail::ArgCheck {
            params,
            opt,
            slurpy,
        } => out.push_str(&format!(
            " argcheck={},{},{}",
            params,
            opt,
            slurpy.map(String::from).unwrap_or_default()
        )),
        OpDetail::ArgElem { index } => out.push_str(&format!(" argelem={}", index)),
        OpDetail::Loop { redo, next, last } => out.push_str(&format!(
            " loop(redo->{:?} next->{:?} last->{:?})",
            redo, next, last
        )),
        OpDetail::Pm { pattern } => out.push_str(&format!(
            " pm={}",
            pattern.as_deref().unwrap_or("<undecoded>")
        )),
        OpDetail::MultiDeref { steps } => {
            out.push_str(" mderef=");
            for s in steps {
                out.push_str(&format!(
                    "{}({}{}){}",
                    if s.container == "array" { "A" } else { "H" },
                    s.base,
                    s.base_targ
                        .map(|t| format!(":{}", t))
                        .or_else(|| s.base_name.clone().map(|n| format!(":{}", n)))
                        .unwrap_or_default(),
                    s.key
                        .as_deref()
                        .map(|k| format!("[{}]", k))
                        .unwrap_or_else(|| "[?]".into()),
                ));
            }
        }
        OpDetail::Aux(s) => out.push_str(&format!(" aux={}", s)),
    }
    out.push('\n');
    for k in &node.kids {
        rec(k, depth + 1, ir, out);
    }
}
