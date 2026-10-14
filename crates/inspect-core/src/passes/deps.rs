//! Dependency facts of a sub: its static `require` sites, and — for a
//! BEGIN block — the `use Module VERSION LIST` / `no Module LIST` shape it
//! was compiled from.
//!
//! `use Foo 1.2 qw(a b)` compiles into a BEGIN whose body is
//! `require Foo; Foo->VERSION(1.2); Foo->import('a', 'b')` (`unimport` for
//! `no`, a stub instead of the method call for `use Foo ()`). [`use_form`] recognizes exactly
//! that shape from the op tree, so the module name does not depend on
//! deparsing.
//!
//! Limits: `require $var` is reported as `dynamic` without a target;
//! `require` inside a string eval is not part of the op tree at all; a
//! version literal is constant-folded to a number (`v5.36.0` and `5.036`
//! look the same, `5.010` comes out as `5.01`).

use serde::{Deserialize, Serialize};

use crate::ir::{OpDetail, OpNode, SubIr, SvLit};

/// One `require` op of a sub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequireSite {
    /// Line of the enclosing statement
    pub line: Option<u32>,
    /// The `%INC` key it loads (`Foo/Bar.pm`, or the string of
    /// `require "x/y.pl"`)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub file: Option<String>,
    /// Package name for a `*.pm` file (`Foo::Bar`)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub module: Option<String>,
    /// `require VERSION` (perl version check, loads nothing)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version: Option<String>,
    /// The argument is not a constant (`require $var`)
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub dynamic: bool,
}

/// What a BEGIN block does when it is a compiled `use` / `no` statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UseForm {
    /// "use" or "no"
    pub kind: String,
    pub module: String,
    /// `use Foo VERSION`
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version: Option<String>,
    /// Arguments of `import` / `unimport`: empty for `use Foo;`, `None`
    /// when the method is not called (`use Foo ()`) or when some argument
    /// is not a constant
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub args: Option<Vec<String>>,
}

/// Every `require` op of the sub, in pre-order.
pub fn require_sites(ir: &SubIr) -> Vec<RequireSite> {
    let mut out = Vec::new();
    super::walk_with_lines(ir, |node, line| {
        if node.name == "require" {
            out.push(require_site(node, line));
        }
    });
    out
}

fn require_site(node: &OpNode, line: Option<u32>) -> RequireSite {
    let mut site = RequireSite {
        line,
        file: None,
        module: None,
        version: None,
        dynamic: false,
    };
    match node.kids.first().map(|k| k.skip_null()).map(|k| &k.detail) {
        Some(OpDetail::Const(SvLit::Pv(s))) => {
            site.module = module_of_file(s);
            site.file = Some(s.clone());
        }
        Some(OpDetail::Const(lit)) => match lit_string(lit) {
            Some(v) => site.version = Some(v),
            None => site.dynamic = true,
        },
        _ => site.dynamic = true,
    }
    site
}

/// `Foo/Bar.pm` → `Foo::Bar` (None for anything that is not a module path)
pub fn module_of_file(file: &str) -> Option<String> {
    let stem = file.strip_suffix(".pm")?;
    let ok = !stem.is_empty()
        && stem
            .split('/')
            .all(|seg| !seg.is_empty() && seg.chars().all(|c| c.is_alphanumeric() || c == '_'));
    ok.then(|| stem.replace('/', "::"))
}

/// The `use` / `no` statement a BEGIN body was compiled from, or `None`
/// when the body has any other shape (an "opaque" BEGIN).
pub fn use_form(ir: &SubIr) -> Option<UseForm> {
    let root = ir.root.skip_null();
    let body = root.kids.first()?;
    let mut stmts = Vec::new();
    flatten_stmts(body, &mut stmts);

    let mut it = stmts.into_iter();
    let first = it.next()?;
    if first.name != "require" {
        return None;
    }
    let site = require_site(first, None);
    let module = site.module?;

    let mut form = UseForm {
        kind: "use".into(),
        module,
        version: None,
        args: None,
    };
    let mut seen_import = false;
    // `use` always compiles something after the require (a method call,
    // or a stub for `use Foo ()`); a lone require is a hand-written
    // `BEGIN { require Foo }`
    let mut followed = false;
    for stmt in it {
        followed = true;
        match stmt.name.as_str() {
            // `use Foo ()`
            "stub" => {}
            "entersub" => {
                let (class, method, args) = method_call(stmt)?;
                if class != form.module || seen_import {
                    return None;
                }
                match method.as_str() {
                    "VERSION" if form.version.is_none() => {
                        let [v] = args?.try_into().ok()?;
                        form.version = Some(v);
                    }
                    "import" | "unimport" => {
                        if method == "unimport" {
                            form.kind = "no".into();
                        }
                        form.args = args;
                        seen_import = true;
                    }
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
    followed.then_some(form)
}

/// Non-COP statements of a block, looking through nested `lineseq`s and
/// nulled wrappers.
fn flatten_stmts<'a>(node: &'a OpNode, out: &mut Vec<&'a OpNode>) {
    for k in &node.kids {
        if matches!(k.detail, OpDetail::Cop { .. }) {
            continue;
        }
        let real = k.skip_null();
        if real.name == "lineseq" || (real.op_type == 0 && !real.kids.is_empty()) {
            flatten_stmts(real, out);
        } else {
            out.push(real);
        }
    }
}

/// `Class->method(CONST, ...)`: (class, method, constant args or None)
fn method_call(entersub: &OpNode) -> Option<(String, String, Option<Vec<String>>)> {
    let mut kids = Vec::new();
    for k in &entersub.kids {
        let k = k.skip_null();
        // the argument list may sit under a nulled ex-list
        if k.op_type == 0 {
            kids.extend(k.kids.iter().map(|kk| kk.skip_null()));
        } else {
            kids.push(k);
        }
    }
    let kids: Vec<&OpNode> = kids.into_iter().filter(|k| k.name != "pushmark").collect();
    let (meth, rest) = kids.split_last()?;
    let OpDetail::Method { name: Some(method) } = &meth.detail else {
        return None;
    };
    let (inv, args) = rest.split_first()?;
    let OpDetail::Const(SvLit::Pv(class)) = &inv.detail else {
        return None;
    };
    let args: Option<Vec<String>> = args
        .iter()
        .map(|a| match &a.detail {
            OpDetail::Const(lit) => lit_string(lit),
            _ => None,
        })
        .collect();
    Some((class.clone(), method.clone(), args))
}

fn lit_string(lit: &SvLit) -> Option<String> {
    match lit {
        SvLit::Pv(s) => Some(s.clone()),
        SvLit::Iv(i) => Some(i.to_string()),
        SvLit::Uv(u) => Some(u.to_string()),
        SvLit::Nv(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::module_of_file;

    #[test]
    fn module_names() {
        assert_eq!(module_of_file("Foo/Bar.pm").as_deref(), Some("Foo::Bar"));
        assert_eq!(module_of_file("strict.pm").as_deref(), Some("strict"));
        assert_eq!(module_of_file("x/y.pl"), None);
        assert_eq!(module_of_file("/abs/Foo.pm"), None);
        assert_eq!(module_of_file("../Foo.pm"), None);
    }
}
