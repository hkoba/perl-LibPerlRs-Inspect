//! Mini deparser — renders small expression subtrees as Perl-like strings.
//!
//! Not a full reimplementation of B::Deparse. A subset aimed at "short
//! expressions shown to humans": default values, return expressions, atomic
//! guard conditions, branch-arm statements, and the like. When adding an op,
//! consult the corresponding pp_* in /usr/share/perl5/B/Deparse.pm.
//!
//! Ops without a dedicated renderer fall back to `name(kid, kid, ...)`
//! (or `<name>` for a leaf), so the output is always informative even when
//! it is not valid Perl. Regex patterns are not captured by the IR, so
//! `match` / `subst` render as `m/.../` / `s/.../REPL/`. Blocks in
//! expression position render as `do { ... }` (only the last statement is
//! shown; elided statements are marked with `...;`), and an `if` / `elsif` /
//! `else` chain with block arms renders in statement form.

use crate::ir::{OpClass, OpDetail, OpNode, SubIr, SvLit};

/// OPf_STACKED
const OPF_STACKED: u8 = 64;
/// OPf_SPECIAL (on OP_NULL: an explicit `do BLOCK`)
const OPF_SPECIAL: u8 = 128;
/// OPpLVAL_INTRO (`my`)
const OPP_LVAL_INTRO: u8 = 128;
/// OPpTARGET_MY
const OPP_TARGET_MY: u8 = 16;
/// OPpMULTICONCAT_APPEND
const OPP_MC_APPEND: u8 = 64;
/// OPpMULTICONCAT_STRINGIFY
const OPP_MC_STRINGIFY: u8 = 8;
/// OPpMULTIDEREF_EXISTS / OPpMULTIDEREF_DELETE
const OPP_MD_EXISTS: u8 = 16;
const OPP_MD_DELETE: u8 = 32;
/// OPpEMPTYAVHV_IS_HV
const OPP_EMPTYAVHV_IS_HV: u8 = 32;

pub fn render(ir: &SubIr, node: &OpNode) -> String {
    let n = node.skip_null();
    match n.name.as_str() {
        // An OP_NULL that skip_null could not see through: a leaf, or a
        // nulled list-like op with several kids (ex-list, ex-aelem, ...).
        // B::Deparse's pp_null likewise deparses the surviving kids.
        "null" => {
            if n.kids.is_empty() {
                return format!("<{}>", n.was.as_deref().unwrap_or("null"));
            }
            let kids = kid_list(ir, n);
            kids.join(", ")
        }
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
        "rv2gv" => match n.kids.first().map(|k| k.skip_null()) {
            Some(k) if is_block(k) => block_body(ir, k),
            Some(k) => render(ir, k),
            None => "<rv2gv>".into(),
        },
        "rv2cv" => match n.kids.first().map(|k| k.skip_null()) {
            Some(k) if k.name == "gv" => match &k.detail {
                OpDetail::Gv { name, stash } => sigil_gv('&', name, stash.as_deref()),
                _ => "<rv2cv>".into(),
            },
            Some(k) => format!("&{{{}}}", render(ir, k)),
            None => "<rv2cv>".into(),
        },
        "aelemfast" => match &n.detail {
            // the index is embedded in op_private
            OpDetail::Gv { name, stash } => {
                let base = sigil_gv('$', name, stash.as_deref());
                format!("{}[{}]", base, n.private as i8)
            }
            _ => "<aelemfast>".into(),
        },
        "aelemfast_lex" => format!(
            "{}[{}]",
            pad_var(ir, n.targ).replacen('@', "$", 1),
            n.private as i8
        ),
        "aelem" => binop_like(ir, n, |a, i| format!("{}[{}]", a.replacen('@', "$", 1), i)),
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
        "repeat" => infix(ir, n, "x"),
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
        "and" | "or" if n.kids.get(1).is_some_and(|k| is_block_arm(k)) => {
            // `if (C) { ... }` / `unless (C) { ... }` without an else
            let kw = if n.name == "and" { "if" } else { "unless" };
            format!("{} ({}) {}", kw, kid_str(ir, n, 0), braces(ir, &n.kids[1]))
        }
        "and" => infix(ir, n, "&&"),
        "or" => infix(ir, n, "||"),
        "dor" => infix(ir, n, "//"),
        "xor" => infix(ir, n, "xor"),
        "not" => match n.kids.first().map(|k| k.skip_null()) {
            Some(k) if k.name == "match" || k.name == "subst" => {
                let s = render(ir, k);
                if s.contains(" =~ ") {
                    s.replacen(" =~ ", " !~ ", 1)
                } else {
                    format!("!{}", s)
                }
            }
            _ => format!("!{}", kid_str(ir, n, 0)),
        },
        "negate" => format!("-{}", kid_str(ir, n, 0)),
        "defined" => format!("defined({})", kid_str(ir, n, 0)),
        "cond_expr" => {
            if n.kids.get(1).is_some_and(|k| is_block_arm(k)) {
                render_if_chain(ir, n)
            } else {
                let c = kid_str(ir, n, 0);
                let t = kid_str(ir, n, 1);
                let f = kid_str(ir, n, 2);
                format!("{} ? {} : {}", c, t, f)
            }
        }
        "sassign" => format!("{} = {}", kid_str(ir, n, 1), kid_str(ir, n, 0)),
        "aassign" => render_aassign(ir, n),
        "shift" | "pop" => {
            if n.kids.is_empty() {
                // a bare shift/pop inside a sub operates on @_
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
            OpDetail::MultiDeref { steps } => {
                let prefix = if n.private & OPP_MD_EXISTS != 0 {
                    "exists "
                } else if n.private & OPP_MD_DELETE != 0 {
                    "delete "
                } else {
                    ""
                };
                let base = if steps.first().is_some_and(|s| s.base == "stack") {
                    real_kids(n).first().map(|k| render(ir, k))
                } else {
                    None
                };
                format!("{}{}", prefix, render_mderef(ir, steps, base.as_deref()))
            }
            _ => "<multideref>".into(),
        },
        "multiconcat" => match &n.detail {
            OpDetail::MultiConcat { pieces } => render_multiconcat(ir, n, pieces),
            _ => generic(ir, n),
        },
        "stringify" => format!("\"{}\"", kid_list(ir, n).join("")),
        "list" | "pushmark" => {
            let kids = real_kids(n);
            if !kids.is_empty() && kids.iter().all(|k| is_my_pad(k)) {
                my_list(ir, &kids)
            } else {
                kids.iter()
                    .map(|k| render(ir, k))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }
        "undef" if n.kids.is_empty() => "undef".into(),
        "argdefelem" => kid_str(ir, n, 0),
        "stub" => "()".into(),
        "print" | "say" | "prtf" => render_print(ir, n),
        "match" | "subst" => render_pm(ir, n),
        "postinc" | "i_postinc" => format!("{}++", kid_str(ir, n, 0)),
        "postdec" | "i_postdec" => format!("{}--", kid_str(ir, n, 0)),
        "preinc" | "i_preinc" => format!("++{}", kid_str(ir, n, 0)),
        "predec" | "i_predec" => format!("--{}", kid_str(ir, n, 0)),
        "substr_left" => {
            // substr(EXPR, 0, LEN): the pushmark and the 0 are nulled leaves
            let kids = kid_list(ir, n);
            format!(
                "substr({}, 0, {})",
                kids.first().cloned().unwrap_or_default(),
                kids.get(1).cloned().unwrap_or_default()
            )
        }
        "grepwhile" | "mapwhile" => render_grep_map(ir, n),
        "anonlist" => format!("[{}]", kid_list(ir, n).join(", ")),
        "anonhash" => format!("{{{}}}", pairs(kid_list(ir, n))),
        "emptyavhv" => {
            let lit = if n.private & OPP_EMPTYAVHV_IS_HV != 0 {
                "{}"
            } else {
                "[]"
            };
            if n.private & OPP_TARGET_MY != 0 {
                let my = if n.private & OPP_LVAL_INTRO != 0 {
                    "my "
                } else {
                    ""
                };
                format!("{}{} = {}", my, pad_var(ir, n.targ), lit)
            } else {
                lit.into()
            }
        }
        "srefgen" | "refgen" => {
            let kids = kid_list(ir, n);
            if kids.len() == 1 {
                format!("\\{}", kids[0])
            } else {
                format!("\\({})", kids.join(", "))
            }
        }
        "leavetry" => format!("eval {{ {} }}", block_body(ir, n)),
        "scope" | "leave" => format!("do {{ {} }}", block_body(ir, n)),
        // the artificial wrapper around an elsif condition
        "lineseq" => block_body(ir, n),
        name if name.starts_with("ft") => match filetest_op(name) {
            Some(sw) => format!(
                "-{} {}",
                sw,
                n.kids
                    .first()
                    .map(|k| render(ir, k))
                    .unwrap_or_else(|| "$_".into())
            ),
            None => generic(ir, n),
        },
        _ => generic(ir, n),
    }
}

/// Fallback for ops without a dedicated renderer.
fn generic(ir: &SubIr, n: &OpNode) -> String {
    if n.kids.is_empty() {
        format!("<{}>", n.name)
    } else {
        format!("{}({})", perl_name(&n.name), kid_list(ir, n).join(", "))
    }
}

/// Op names that differ from the Perl function they implement.
fn perl_name(op: &str) -> &str {
    match op {
        "prtf" => "printf",
        "open_dir" => "opendir",
        other => other,
    }
}

/// B::Deparse `ftst` switch letters for the file test ops seen in practice.
fn filetest_op(name: &str) -> Option<char> {
    Some(match name {
        "ftdir" => 'd',
        "ftlink" => 'l',
        "ftfile" => 'f',
        "ftis" => 'e',
        "ftsize" => 's',
        "ftzero" => 'z',
        "fteread" => 'r',
        "ftewrite" => 'w',
        "fteexec" => 'x',
        _ => return None,
    })
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

/// Escape a string for display inside double quotes. `interp` also
/// escapes `$` / `@` (for pieces of an interpolated string).
fn escape_str(s: &str, interp: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '$' | '@' if interp => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

fn lit_str(lit: &SvLit) -> String {
    match lit {
        SvLit::Undef => "undef".into(),
        SvLit::Iv(i) => i.to_string(),
        SvLit::Uv(u) => u.to_string(),
        SvLit::Nv(n) => n.to_string(),
        SvLit::Pv(s) => format!("\"{}\"", escape_str(s, false)),
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
        Some(k) if is_block(k) => format!("{}{{{}}}", sigil, block_body(ir, k)),
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

/// A nulled op with nothing underneath (ex-const, ex-pushmark, ex-padsv...).
fn is_leaf_null(k: &OpNode) -> bool {
    k.op_type == 0 && k.kids.is_empty()
}

/// The children that carry a value: nulls skipped, and pushmark / padrange
/// / nulled leaves dropped.
pub(crate) fn real_kids(n: &OpNode) -> Vec<&OpNode> {
    n.kids
        .iter()
        .map(|k| k.skip_null())
        .filter(|k| k.name != "pushmark" && k.name != "padrange" && !is_leaf_null(k))
        .collect()
}

/// `real_kids`, rendered.
pub(crate) fn kid_list(ir: &SubIr, n: &OpNode) -> Vec<String> {
    real_kids(n).into_iter().map(|k| render(ir, k)).collect()
}

fn is_block(n: &OpNode) -> bool {
    matches!(n.name.as_str(), "scope" | "leave")
}

/// A branch arm that is a real block (not an explicit `do BLOCK`, which
/// sits under an OP_NULL flagged OPf_SPECIAL).
fn is_block_arm(raw: &OpNode) -> bool {
    !(raw.op_type == 0 && raw.flags & OPF_SPECIAL != 0) && is_block(raw.skip_null())
}

/// The statements of a block op, without COPs and enter / entertry / unstack.
fn block_stmts(n: &OpNode) -> Vec<&OpNode> {
    n.kids
        .iter()
        .filter(|k| {
            k.class != OpClass::Cop && !matches!(k.name.as_str(), "enter" | "entertry" | "unstack")
        })
        .collect()
}

/// The visible body of a block: its last statement, with `...;` marking
/// any elided statements before it.
fn block_body(ir: &SubIr, n: &OpNode) -> String {
    let stmts = block_stmts(n);
    match stmts.len() {
        0 => String::new(),
        1 => render(ir, stmts[0]),
        _ => format!("...; {}", render(ir, stmts[stmts.len() - 1])),
    }
}

fn braces(ir: &SubIr, arm: &OpNode) -> String {
    let body = block_body(ir, arm.skip_null());
    if body.is_empty() {
        "{}".into()
    } else {
        format!("{{ {} }}", body)
    }
}

/// `if (C) { ... } elsif (C2) { ... } else { ... }` for a cond_expr whose
/// arms are blocks (B::Deparse pp_cond_expr).
fn render_if_chain(ir: &SubIr, n: &OpNode) -> String {
    let mut out = format!("if ({}) {}", kid_str(ir, n, 0), braces(ir, &n.kids[1]));
    let mut cur = n.kids.get(2);
    while let Some(f) = cur {
        let fs = f.skip_null();
        if fs.name == "cond_expr" && fs.kids.get(1).is_some_and(|k| is_block_arm(k)) {
            out.push_str(&format!(
                " elsif ({}) {}",
                kid_str(ir, fs, 0),
                braces(ir, &fs.kids[1])
            ));
            cur = fs.kids.get(2);
        } else if fs.name == "and" && fs.kids.get(1).is_some_and(|k| is_block_arm(k)) {
            out.push_str(&format!(
                " elsif ({}) {}",
                kid_str(ir, fs, 0),
                braces(ir, &fs.kids[1])
            ));
            break;
        } else if is_block_arm(f) {
            out.push_str(&format!(" else {}", braces(ir, f)));
            break;
        } else {
            out.push_str(&format!(" else {{ {} }}", render(ir, f)));
            break;
        }
    }
    out
}

/// A pad variable being introduced with `my`.
fn is_my_pad(k: &OpNode) -> bool {
    k.name.starts_with("pad") && k.private & OPP_LVAL_INTRO != 0
}

fn my_list(ir: &SubIr, kids: &[&OpNode]) -> String {
    let names: Vec<String> = kids.iter().map(|k| render(ir, k)).collect();
    if names.len() == 1 {
        format!("my {}", names[0])
    } else {
        format!("my ({})", names.join(", "))
    }
}

fn is_aggregate(k: &OpNode) -> bool {
    matches!(k.name.as_str(), "padav" | "padhv" | "rv2av" | "rv2hv")
}

/// Items of a list-like node (list / nulled ex-list), or the node itself.
fn list_items(n: &OpNode) -> Vec<&OpNode> {
    let s = n.skip_null();
    if s.name == "list" || (s.name == "null" && !s.kids.is_empty()) {
        real_kids(s)
    } else {
        vec![s]
    }
}

/// aassign: kids are [RHS, LHS].
fn render_aassign(ir: &SubIr, n: &OpNode) -> String {
    let (Some(rhs), Some(lhs)) = (n.kids.first(), n.kids.get(1)) else {
        return generic(ir, n);
    };
    let l = list_items(lhs);
    let lhs_s = if !l.is_empty() && l.iter().all(|k| is_my_pad(k)) {
        if l.len() == 1 && is_aggregate(l[0]) {
            format!("my {}", render(ir, l[0]))
        } else {
            format!(
                "my ({})",
                l.iter()
                    .map(|k| render(ir, k))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    } else if l.len() == 1 && is_aggregate(l[0]) {
        render(ir, l[0])
    } else {
        format!(
            "({})",
            l.iter()
                .map(|k| render(ir, k))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let r = list_items(rhs);
    let rhs_s = if r.len() == 1 {
        render(ir, r[0])
    } else {
        format!(
            "({})",
            r.iter()
                .map(|k| render(ir, k))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!("{} = {}", lhs_s, rhs_s)
}

/// print / say / printf, with the optional filehandle (OPf_STACKED).
fn render_print(ir: &SubIr, n: &OpNode) -> String {
    let mut out = perl_name(&n.name).to_string();
    let mut args = real_kids(n);
    let mut has_fh = false;
    if n.flags & OPF_STACKED != 0 && args.first().is_some_and(|k| k.name == "rv2gv") {
        let fh = args.remove(0);
        has_fh = true;
        out.push(' ');
        match fh.kids.first().map(|k| k.skip_null()) {
            Some(k) if k.name == "gv" => match &k.detail {
                OpDetail::Gv { name, stash } => {
                    out.push_str(&sigil_gv_bare(name, stash.as_deref()))
                }
                _ => out.push_str("<fh>"),
            },
            Some(k) if is_block(k) => out.push_str(&format!("{{{}}}", block_body(ir, k))),
            Some(k) => out.push_str(&format!("{{{}}}", render(ir, k))),
            None => out.push_str("<fh>"),
        }
    }
    if !args.is_empty() {
        out.push(' ');
        out.push_str(
            &args
                .iter()
                .map(|k| render(ir, k))
                .collect::<Vec<_>>()
                .join(", "),
        );
    } else if !has_fh {
        out.push_str("()");
    }
    out
}

fn sigil_gv_bare(name: &str, stash: Option<&str>) -> String {
    match stash {
        Some("main") | None => name.to_string(),
        Some(pkg) => format!("{}::{}", pkg, name),
    }
}

/// match / subst. The pattern is not available in the IR.
fn render_pm(ir: &SubIr, n: &OpNode) -> String {
    let mut args = real_kids(n);
    let target = if n.flags & OPF_STACKED != 0 && !args.is_empty() {
        Some(render(ir, args.remove(0)))
    } else if n.targ != 0 {
        Some(pad_var(ir, n.targ))
    } else {
        None
    };
    let body = if n.name == "match" {
        "m/.../".to_string()
    } else {
        let repl = args
            .iter()
            .find(|k| k.name != "regcomp")
            .map(|k| match &k.detail {
                OpDetail::Const(SvLit::Pv(s)) if k.name == "const" => s.clone(),
                _ => render(ir, k),
            })
            .unwrap_or_default();
        format!("s/.../{}/", repl)
    };
    match target {
        Some(t) => format!("{} =~ {}", t, body),
        None => body,
    }
}

/// grep / map: `grep { BODY } LIST` or `grep EXPR, LIST`.
fn render_grep_map(ir: &SubIr, n: &OpNode) -> String {
    let name = if n.name == "grepwhile" { "grep" } else { "map" };
    let Some(start) = n.kids.first().map(|k| k.skip_null()) else {
        return generic(ir, n);
    };
    let kids: Vec<&OpNode> = start
        .kids
        .iter()
        .map(|k| k.skip_null())
        .filter(|k| k.name != "pushmark")
        .collect();
    let Some((code, list)) = kids.split_first() else {
        return generic(ir, n);
    };
    let code_s = if is_block(code) {
        format!("{{ {} }}", block_body(ir, code))
    } else {
        format!("{},", render(ir, code))
    };
    let list_s = list
        .iter()
        .map(|k| render(ir, k))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{} {} {}", name, code_s, list_s)
}

/// `k => v` pairs when the item count is even, otherwise a plain list.
fn pairs(items: Vec<String>) -> String {
    if !items.is_empty() && items.len() % 2 == 0 {
        items
            .chunks(2)
            .map(|p| format!("{} => {}", p[0], p[1]))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        items.join(", ")
    }
}

/// Something that can appear inside an interpolated string as is.
fn interpolable(s: &str) -> bool {
    s.starts_with('$') && !s.contains(char::is_whitespace)
}

/// multiconcat: `"a$x b"`, `"p: " . $x . "\n"`, `$x .= $y`, `my $s = "a$x"`.
fn render_multiconcat(ir: &SubIr, n: &OpNode, pieces: &[Option<String>]) -> String {
    let priv_ = n.private;
    let append = priv_ & OPP_MC_APPEND != 0;
    let mut args: Vec<String> = kid_list(ir, n);
    let lhs = if priv_ & OPP_TARGET_MY != 0 {
        let v = pad_var(ir, n.targ);
        Some(if priv_ & OPP_LVAL_INTRO != 0 {
            format!("my {}", v)
        } else {
            v
        })
    } else if n.flags & OPF_STACKED != 0 && !args.is_empty() {
        Some(if append {
            args.remove(0)
        } else {
            args.pop().unwrap()
        })
    } else {
        None
    };
    let stringify = priv_ & OPP_MC_STRINGIFY != 0;
    let rhs = if stringify && args.iter().all(|a| interpolable(a)) {
        let mut s = String::from("\"");
        for (i, a) in args.iter().enumerate() {
            if let Some(Some(p)) = pieces.get(i) {
                s.push_str(&escape_str(p, true));
            }
            s.push_str(a);
        }
        if let Some(Some(p)) = pieces.get(args.len()) {
            s.push_str(&escape_str(p, true));
        }
        s.push('"');
        s
    } else {
        let mut parts: Vec<String> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if let Some(Some(p)) = pieces.get(i) {
                if !p.is_empty() {
                    parts.push(format!("\"{}\"", escape_str(p, false)));
                }
            }
            parts.push(a.clone());
        }
        if let Some(Some(p)) = pieces.get(args.len()) {
            if !p.is_empty() {
                parts.push(format!("\"{}\"", escape_str(p, false)));
            }
        }
        parts.join(" . ")
    };
    match lhs {
        Some(l) => format!("{} {} {}", l, if append { ".=" } else { "=" }, rhs),
        None => rhs,
    }
}

/// Render a multideref chain: `$x->[0]{k}` / `$arr[0]` / `$h{k}` etc.
/// `base` is the rendered expression a `stack`-based chain applies to.
pub(crate) fn render_mderef(
    ir: &SubIr,
    steps: &[crate::ir::DerefStep],
    base: Option<&str>,
) -> String {
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
                // $x->[...] : deref of a lexical holding a ref
                let name = s
                    .base_targ
                    .and_then(|t| ir.pad_name(t).map(String::from))
                    .unwrap_or_else(|| "$?".into());
                out.push_str(&format!("{}->{}", name, subscript(&s.key)));
            }
            "padav" | "padhv" => {
                // $arr[...] / $h{...} : direct element access on an aggregate lexical
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
            // chain: subscript on the previous step's result (arrow-omitted form)
            "chain" if i > 0 => out.push_str(&subscript(&s.key)),
            // stack: the chain applies to the result of a preceding op
            _ => out.push_str(&format!(
                "{}->{}",
                base.unwrap_or("<expr>"),
                subscript(&s.key)
            )),
        }
    }
    if out.is_empty() {
        "<multideref>".into()
    } else {
        out
    }
}

/// Render an entersub: `f(args)` / `$obj->meth(args)` / `$code->(args)`
fn render_call(ir: &SubIr, n: &OpNode) -> String {
    // look through the entersub's children (skipping nulls) for the callee
    // (a GV, or the expression under a nulled rv2cv) and the method op
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
        for raw in &n.kids {
            let k = raw.skip_null();
            if raw.op_type == 0 && raw.was.as_deref() == Some("rv2cv") && k.name != "gv" {
                // `$code->(...)` / `&$code(...)`: the callee is an expression
                *callee = Some(format!("{}->", render(ir, k)));
                continue;
            }
            match k.name.as_str() {
                "pushmark" | "padrange" => {}
                "null" => scan(ir, k, args, callee, method),
                "gv" => {
                    if let OpDetail::Gv { name, stash } = &k.detail {
                        *callee = Some(sigil_gv_bare(name, stash.as_deref()));
                    }
                }
                "method_named" | "method" | "method_super" | "method_redir"
                | "method_redir_super" => {
                    let m = match &k.detail {
                        OpDetail::Method { name: Some(m) } => m.clone(),
                        _ => "<dynamic>".into(),
                    };
                    *method = Some(if k.name.ends_with("super") {
                        format!("SUPER::{}", m)
                    } else {
                        m
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ir_of(golden: &str) -> SubIr {
        serde_json::from_str(golden).expect("golden fixture deserializes")
    }

    /// The last statement of the sub body.
    fn last_stmt(ir: &SubIr) -> &OpNode {
        crate::passes::statements(ir)
            .last()
            .map(|(_, n)| *n)
            .unwrap()
    }

    #[test]
    fn print_chain_renders_as_if_chain() {
        let ir = ir_of(include_str!("../../../t/golden/5.42-threaded/print_chain.json"));
        assert_eq!(
            render(&ir, last_stmt(&ir)),
            "if ($x < 0) { print \"x < 0\" } elsif ($y < 0) { print \"y < 0\" } \
             elsif ($z < 0) { print \"z < 0\" } else { print \"x,y,z >= 0\" }"
        );
    }

    #[test]
    fn interpolated_string() {
        let ir = ir_of(include_str!("../../../t/golden/5.42-threaded/interp.json"));
        assert_eq!(render(&ir, last_stmt(&ir)), "\"a$x b\"");
    }

    #[test]
    fn escapes() {
        assert_eq!(escape_str("a\"b\\c\n", false), "a\\\"b\\\\c\\n");
        assert_eq!(escape_str("$x @y", true), "\\$x \\@y");
        assert_eq!(pairs(vec!["\"a\"".into(), "1".into()]), "\"a\" => 1");
        assert_eq!(pairs(vec!["1".into()]), "1");
    }
}
