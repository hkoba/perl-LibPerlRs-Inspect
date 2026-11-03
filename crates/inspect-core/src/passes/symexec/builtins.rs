//! Operators and builtins on symbolic values: evaluated on the spot when
//! the operands are constants, kept as [`Sym::App`] otherwise.

use super::value::{HashSeg, Sym};
use crate::ir::SvLit;

/// The operator symbol of a pure scalar op, by op name.
pub fn op_symbol(name: &str) -> Option<&'static str> {
    Some(match name {
        "eq" | "i_eq" => "==",
        "ne" | "i_ne" => "!=",
        "lt" | "i_lt" => "<",
        "gt" | "i_gt" => ">",
        "le" | "i_le" => "<=",
        "ge" | "i_ge" => ">=",
        "ncmp" | "i_ncmp" => "<=>",
        "seq" => "eq",
        "sne" => "ne",
        "slt" => "lt",
        "sgt" => "gt",
        "sle" => "le",
        "sge" => "ge",
        "scmp" => "cmp",
        "add" | "i_add" => "+",
        "subtract" | "i_subtract" => "-",
        "multiply" | "i_multiply" => "*",
        "divide" | "i_divide" => "/",
        "modulo" | "i_modulo" => "%",
        "concat" => ".",
        "not" => "not",
        "defined" => "defined",
        "ref" => "ref",
        "lc" => "lc",
        "uc" => "uc",
        "lcfirst" => "lcfirst",
        "ucfirst" => "ucfirst",
        "length" => "length",
        "substr" => "substr",
        "negate" | "i_negate" => "neg",
        "abs" => "abs",
        "int" => "int",
        "stringify" => "stringify",
        "join" => "join",
        "index" => "index",
        "sprintf" => "sprintf",
        "blessed" => "blessed",
        _ => return None,
    })
}

fn yes() -> Sym {
    Sym::iv(1)
}

fn no() -> Sym {
    Sym::pv("")
}

fn bool_sym(b: bool) -> Sym {
    if b { yes() } else { no() }
}

fn num_sym(n: f64) -> Sym {
    if n.fract() == 0.0 && n.abs() < 9.0e15 {
        Sym::iv(n as i64)
    } else {
        Sym::Const(SvLit::Nv(n))
    }
}

/// Apply operator `op` (a symbol from [`op_symbol`]) to the operands.
pub fn apply(op: &'static str, args: Vec<Sym>) -> Sym {
    if let Some(v) = fold(op, &args) {
        return v;
    }
    Sym::App(op, args)
}

fn fold(op: &str, a: &[Sym]) -> Option<Sym> {
    match (op, a) {
        ("not", [x]) => x.truth().map(|t| bool_sym(!t)),
        ("defined", [x]) => match x {
            Sym::Const(SvLit::Undef) => Some(no()),
            Sym::Const(_) | Sym::Ref(_) | Sym::RefPad { .. } | Sym::Code(_) => Some(yes()),
            _ => None,
        },
        ("ref", [x]) => match x {
            Sym::Ref(inner) => Some(Sym::pv(match inner.as_ref() {
                Sym::List { .. } => "ARRAY",
                Sym::Hash { .. } => "HASH",
                Sym::Code(_) => "CODE",
                _ => "SCALAR",
            })),
            Sym::RefPad { .. } => Some(Sym::pv("SCALAR")),
            Sym::Code(_) => Some(Sym::pv("CODE")),
            Sym::Const(SvLit::RefTo(_) | SvLit::Code | SvLit::Other(_)) => None,
            Sym::Const(_) => Some(no()),
            _ => None,
        },
        ("==" | "!=" | "<" | ">" | "<=" | ">=" | "<=>", [l, r]) => {
            let (l, r) = (l.const_num()?, r.const_num()?);
            Some(match op {
                "==" => bool_sym(l == r),
                "!=" => bool_sym(l != r),
                "<" => bool_sym(l < r),
                ">" => bool_sym(l > r),
                "<=" => bool_sym(l <= r),
                ">=" => bool_sym(l >= r),
                _ => num_sym(l.partial_cmp(&r).map_or(0.0, |o| o as i32 as f64)),
            })
        }
        ("eq" | "ne" | "lt" | "gt" | "le" | "ge" | "cmp", [l, r]) => {
            let (l, r) = (l.const_str()?, r.const_str()?);
            Some(match op {
                "eq" => bool_sym(l == r),
                "ne" => bool_sym(l != r),
                "lt" => bool_sym(l < r),
                "gt" => bool_sym(l > r),
                "le" => bool_sym(l <= r),
                "ge" => bool_sym(l >= r),
                _ => num_sym(l.cmp(&r) as i32 as f64),
            })
        }
        ("+" | "-" | "*" | "/" | "%", [l, r]) => {
            let (l, r) = (l.const_num()?, r.const_num()?);
            Some(num_sym(match op {
                "+" => l + r,
                "-" => l - r,
                "*" => l * r,
                "/" if r != 0.0 => l / r,
                "%" if r != 0.0 => (l as i64 % r as i64) as f64,
                _ => return None,
            }))
        }
        ("neg", [x]) => Some(num_sym(-x.const_num()?)),
        ("abs", [x]) => Some(num_sym(x.const_num()?.abs())),
        ("int", [x]) => Some(num_sym(x.const_num()?.trunc())),
        (".", parts) => {
            let mut s = String::new();
            for p in parts {
                s.push_str(&p.const_str()?);
            }
            Some(Sym::pv(&s))
        }
        ("stringify", [x]) => Some(Sym::pv(&x.const_str()?)),
        ("lc", [x]) => Some(Sym::pv(&x.const_str()?.to_lowercase())),
        ("uc", [x]) => Some(Sym::pv(&x.const_str()?.to_uppercase())),
        ("length", [x]) => match x {
            Sym::Const(SvLit::Undef) => Some(Sym::undef()),
            _ => Some(Sym::iv(x.const_str()?.chars().count() as i64)),
        },
        ("substr", [s, off, rest @ ..]) => {
            let s: Vec<char> = s.const_str()?.chars().collect();
            let n = s.len() as i64;
            let mut o = off.const_int()?;
            if o < 0 {
                o += n;
            }
            let o = o.clamp(0, n) as usize;
            let len = match rest.first() {
                Some(l) => l.const_int()?,
                None => n,
            };
            let end = if len < 0 {
                (n + len).max(o as i64) as usize
            } else {
                (o + len as usize).min(s.len())
            };
            Some(Sym::pv(&s[o..end].iter().collect::<String>()))
        }
        _ => None,
    }
}

/// Whether a value is a hash without any flattened list (all its keys
/// are known).
pub fn is_concrete_hash(h: &Sym) -> bool {
    matches!(h, Sym::Hash { segs } if segs.iter().all(|s| matches!(s, HashSeg::Pairs(_))))
}
