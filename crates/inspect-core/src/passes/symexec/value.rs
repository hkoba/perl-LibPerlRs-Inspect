//! Symbolic values of the abstract executor.
//!
//! A [`Sym`] stands for a perl value in terms of the analyzed sub's `@_`
//! (`Arg`, `ArgBack`, `ArgRest`, `ArgMid`), constants, and structured
//! values built from them. Values the executor cannot follow become
//! [`Sym::Unknown`] and evaluation goes on.

use crate::ir::SvLit;

/// A symbolic value.
#[derive(Debug, Clone, PartialEq)]
pub enum Sym {
    Const(SvLit),
    /// `$_[i]` of the analyzed sub (counted from the front of the
    /// original `@_`).
    Arg(u32),
    /// The i-th element from the back of the original `@_` (0 = last).
    ArgBack(u32),
    /// The list `@_[i..]` of the original `@_`.
    ArgRest(u32),
    /// The list of the original `@_` without its first `.0` and its last
    /// `.1` elements (`.1` > 0; with `.1` = 0 it is [`Sym::ArgRest`]).
    ArgMid(u32, u32),
    /// A list value: known leading items, then an optional list of
    /// unknown length.
    List {
        items: Vec<Sym>,
        tail: Option<Box<Sym>>,
    },
    /// A hash built from (in order) key/value pairs and flattened lists;
    /// later segments override earlier ones.
    Hash { segs: Vec<HashSeg> },
    /// A reference to a concrete value (`[...]`, `{...}`, `\@x`).
    Ref(Box<Sym>),
    /// A reference to a pad variable of the given frame (`\$x`).
    RefPad { frame: u32, targ: u64 },
    /// Number of elements of a list (`scalar(@_)`).
    Len(Box<Sym>),
    /// Element of an array / hash value.
    Elem(Box<Sym>, Key),
    /// Dereference of a reference whose target is not known
    /// (`@$x` / `%$x` / `$$x`, or the container of `$x->{k}`).
    Deref(Box<Sym>, char),
    /// `keys %h` of a symbolic hash.
    Keys(Box<Sym>),
    /// An operator / builtin applied to symbolic operands (`ref`, `eq`,
    /// `defined`, `not`, `concat`, …).
    App(&'static str, Vec<Sym>),
    /// A value with a fallback: `$x // d`, `$x || d`, `@_ ? shift : d`,
    /// `$x //= d`, `$x = d unless defined $x`.
    Default {
        arg: Box<Sym>,
        default: Box<Sym>,
        when: DefaultWhen,
    },
    /// Result of a call that was not executed.
    Call {
        callee: String,
        args: Vec<Sym>,
        method: bool,
    },
    /// A code reference (`sub {...}`, keyed by the node id of the
    /// `anoncode` constant when known).
    Code(Option<u32>),
    /// Merged value at a join point: alternatives with the branch
    /// conditions under which they hold.
    Join(Vec<(Vec<Cond>, Sym)>),
    Unknown(&'static str),
}

/// When a [`Sym::Default`] falls back to its default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultWhen {
    /// the value is undef (`//`, `//=`, `unless defined`)
    Undef,
    /// the value is false (`||`, `||=`)
    False,
    /// the argument is missing (`@_ ? shift : d`, a hash key absent from
    /// the arguments)
    Missing,
}

/// One segment of a [`Sym::Hash`].
#[derive(Debug, Clone, PartialEq)]
pub enum HashSeg {
    Pairs(Vec<(String, Sym)>),
    /// a flattened list (`%h = (@_)`) or hash
    Rest(Sym),
}

/// Index or key of an element access.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    Index(i64),
    Str(String),
    Sym(Box<Sym>),
    /// an element in general (the loop variable over a list of unknown
    /// length)
    Any(Constraint),
}

/// What is known about a general element key.
#[derive(Debug, Clone, PartialEq)]
pub enum Constraint {
    Unconstrained,
    /// the key equals `eq` after the normalizations
    Matches { norm: Vec<Norm>, eq: String },
    /// the key, normalized, is none of `keys`
    NotIn { norm: Vec<Norm>, keys: Vec<String> },
}

/// A key normalization applied before a lookup.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Norm {
    Lc,
    StripPrefix(String),
}

/// A branch condition: `sym` is true (`truth`) or false.
#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    pub sym: Sym,
    pub truth: bool,
}

impl Cond {
    /// A condition with `not` peeled off its value.
    pub fn new(sym: Sym, truth: bool) -> Cond {
        match sym {
            Sym::App("not", mut v) if v.len() == 1 => Cond::new(v.pop().unwrap(), !truth),
            sym => Cond { sym, truth },
        }
    }

    pub fn negate(&self) -> Cond {
        Cond {
            sym: self.sym.clone(),
            truth: !self.truth,
        }
    }

    pub fn display(&self) -> String {
        if self.truth {
            self.sym.display()
        } else {
            format!("!{}", self.sym.display())
        }
    }
}

impl Sym {
    pub fn undef() -> Sym {
        Sym::Const(SvLit::Undef)
    }

    pub fn iv(i: i64) -> Sym {
        Sym::Const(SvLit::Iv(i))
    }

    pub fn pv(s: &str) -> Sym {
        Sym::Const(SvLit::Pv(s.to_string()))
    }

    /// The empty list.
    pub fn empty() -> Sym {
        Sym::List {
            items: Vec::new(),
            tail: None,
        }
    }

    /// `@_[from..$#_-back]` of the original `@_`.
    pub fn arg_slice(from: u32, back: u32) -> Sym {
        if back == 0 {
            Sym::ArgRest(from)
        } else {
            Sym::ArgMid(from, back)
        }
    }

    /// `(from, back)` of an `ArgRest` / `ArgMid`.
    pub fn as_arg_slice(&self) -> Option<(u32, u32)> {
        match self {
            Sym::ArgRest(f) => Some((*f, 0)),
            Sym::ArgMid(f, b) => Some((*f, *b)),
            _ => None,
        }
    }

    pub fn is_const(&self) -> bool {
        matches!(self, Sym::Const(_))
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Sym::Unknown(_))
    }

    /// Whether the value depends on the analyzed sub's arguments.
    pub fn mentions_args(&self) -> bool {
        match self {
            Sym::Arg(_) | Sym::ArgBack(_) | Sym::ArgRest(_) | Sym::ArgMid(..) => true,
            Sym::Const(_) | Sym::RefPad { .. } | Sym::Code(_) | Sym::Unknown(_) => false,
            Sym::List { items, tail } => {
                items.iter().any(Sym::mentions_args)
                    || tail.as_deref().is_some_and(Sym::mentions_args)
            }
            Sym::Hash { segs } => segs.iter().any(|s| match s {
                HashSeg::Pairs(p) => p.iter().any(|(_, v)| v.mentions_args()),
                HashSeg::Rest(r) => r.mentions_args(),
            }),
            Sym::Ref(x) | Sym::Len(x) | Sym::Deref(x, _) | Sym::Keys(x) => x.mentions_args(),
            Sym::Elem(x, k) => x.mentions_args() || k.mentions_args(),
            Sym::App(_, v) => v.iter().any(Sym::mentions_args),
            Sym::Default { arg, default, .. } => arg.mentions_args() || default.mentions_args(),
            Sym::Call { args, .. } => args.iter().any(Sym::mentions_args),
            Sym::Join(alts) => alts.iter().any(|(_, s)| s.mentions_args()),
        }
    }

    /// Truth value of a constant, if known.
    pub fn truth(&self) -> Option<bool> {
        match self {
            Sym::Const(lit) => lit_truth(lit),
            Sym::Ref(_) | Sym::RefPad { .. } | Sym::Code(_) => Some(true),
            Sym::List { items, tail: None } => Some(!items.is_empty()),
            Sym::Hash { segs } if segs.iter().all(|s| matches!(s, HashSeg::Pairs(_))) => {
                Some(segs.iter().any(|s| matches!(s, HashSeg::Pairs(p) if !p.is_empty())))
            }
            _ => None,
        }
    }

    /// String value of a constant (perl stringification of plain
    /// scalars).
    pub fn const_str(&self) -> Option<String> {
        match self {
            Sym::Const(lit) => lit_str(lit),
            _ => None,
        }
    }

    /// Numeric value of a constant.
    pub fn const_num(&self) -> Option<f64> {
        match self {
            Sym::Const(lit) => lit_num(lit),
            _ => None,
        }
    }

    /// Integer value of a constant (numeric strings included).
    pub fn const_int(&self) -> Option<i64> {
        let n = self.const_num()?;
        (n.fract() == 0.0 && n.abs() < 9.0e15).then_some(n as i64)
    }

    /// A perl-like rendering for display (`$_[0]`, `@_[2..]`,
    /// `ref($_[1])`, `$_[1]->{verbose}`).
    pub fn display(&self) -> String {
        match self {
            Sym::Const(lit) => lit_display(lit),
            Sym::Arg(i) => format!("$_[{}]", i),
            Sym::ArgBack(i) => format!("$_[-{}]", i + 1),
            Sym::ArgRest(0) => "@_".into(),
            Sym::ArgRest(i) => format!("@_[{}..]", i),
            Sym::ArgMid(i, b) => format!("@_[{}..$#_-{}]", i, b),
            Sym::List { items, tail } => {
                let mut parts: Vec<String> = items.iter().map(Sym::display).collect();
                if let Some(t) = tail {
                    parts.push(t.display());
                }
                format!("({})", parts.join(", "))
            }
            Sym::Hash { segs } => {
                let parts: Vec<String> = segs
                    .iter()
                    .flat_map(|s| match s {
                        HashSeg::Pairs(p) => p
                            .iter()
                            .map(|(k, v)| format!("{} => {}", k, v.display()))
                            .collect::<Vec<_>>(),
                        HashSeg::Rest(r) => vec![r.display()],
                    })
                    .collect();
                format!("({})", parts.join(", "))
            }
            Sym::Ref(x) => match x.as_ref() {
                Sym::Hash { .. } => format!("{{{}}}", x.display().trim_matches(['(', ')'])),
                Sym::List { .. } => format!("[{}]", x.display().trim_matches(['(', ')'])),
                _ => format!("\\{}", x.display()),
            },
            Sym::RefPad { targ, .. } => format!("\\<pad {}>", targ),
            Sym::Len(x) => format!("scalar({})", x.display()),
            Sym::Elem(base, key) => {
                let k = key.display();
                match base.as_ref() {
                    Sym::Hash { segs } => match segs.as_slice() {
                        [HashSeg::Rest(r)] => format!("({}){{{}}}", r.display(), k),
                        _ => format!("{}{{{}}}", base.display(), k),
                    },
                    Sym::Deref(r, '%') => format!("{}->{{{}}}", r.display(), k),
                    Sym::Deref(r, '@') => format!("{}->[{}]", r.display(), k),
                    b @ Sym::List { .. } => format!("{}[{}]", b.display(), k),
                    b if b.as_arg_slice().is_some() => format!("({})[{}]", b.display(), k),
                    b => format!("{}[{}]", b.display(), k),
                }
            }
            Sym::Deref(x, c) => format!("{}{{{}}}", c, x.display()),
            Sym::Keys(x) => format!("keys({})", x.display()),
            Sym::App(op, v) => {
                let args: Vec<String> = v.iter().map(Sym::display).collect();
                match (*op, args.as_slice()) {
                    (op, [l, r]) if is_infix(op) => format!("({} {} {})", l, op, r),
                    ("not", [x]) => format!("!{}", x),
                    (op, args) => format!("{}({})", op, args.join(", ")),
                }
            }
            Sym::Default { arg, default, when } => {
                let op = match when {
                    DefaultWhen::Undef => "//",
                    DefaultWhen::False => "||",
                    DefaultWhen::Missing => "?:",
                };
                format!("{} {} {}", arg.display(), op, default.display())
            }
            Sym::Call {
                callee,
                args,
                method,
            } => {
                let a: Vec<String> = args.iter().map(Sym::display).collect();
                if *method && !a.is_empty() {
                    format!("{}->{}({})", a[0], callee, a[1..].join(", "))
                } else {
                    format!("{}({})", callee, a.join(", "))
                }
            }
            Sym::Code(_) => "sub {...}".into(),
            Sym::Join(alts) => {
                let a: Vec<String> = alts.iter().map(|(_, s)| s.display()).collect();
                format!("<{}>", a.join(" | "))
            }
            Sym::Unknown(what) => format!("<{}>", what),
        }
    }
}

impl Key {
    fn mentions_args(&self) -> bool {
        match self {
            Key::Sym(s) => s.mentions_args(),
            _ => false,
        }
    }

    pub fn display(&self) -> String {
        match self {
            Key::Index(i) => i.to_string(),
            Key::Str(s) => s.clone(),
            Key::Sym(s) => s.display(),
            Key::Any(Constraint::Unconstrained) => "*".into(),
            Key::Any(Constraint::Matches { eq, .. }) => format!("~{}", eq),
            Key::Any(Constraint::NotIn { .. }) => "*".into(),
        }
    }
}

fn is_infix(op: &str) -> bool {
    matches!(
        op,
        "==" | "!=" | "<" | ">" | "<=" | ">=" | "eq" | "ne" | "lt" | "gt" | "le" | "ge"
            | "+" | "-" | "*" | "/" | "%" | "." | "&&" | "||" | "//" | "<=>" | "cmp"
    )
}

pub(crate) fn lit_truth(lit: &SvLit) -> Option<bool> {
    match lit {
        SvLit::Undef => Some(false),
        SvLit::Iv(i) => Some(*i != 0),
        SvLit::Uv(u) => Some(*u != 0),
        SvLit::Nv(n) => Some(*n != 0.0),
        SvLit::Pv(s) => Some(!(s.is_empty() || s == "0")),
        SvLit::RefTo(_) | SvLit::Code | SvLit::Glob { .. } => Some(true),
        SvLit::Other(_) => None,
    }
}

pub(crate) fn lit_str(lit: &SvLit) -> Option<String> {
    match lit {
        SvLit::Undef => Some(String::new()),
        SvLit::Iv(i) => Some(i.to_string()),
        SvLit::Uv(u) => Some(u.to_string()),
        SvLit::Nv(n) => Some(n.to_string()),
        SvLit::Pv(s) => Some(s.clone()),
        _ => None,
    }
}

pub(crate) fn lit_num(lit: &SvLit) -> Option<f64> {
    match lit {
        SvLit::Undef => Some(0.0),
        SvLit::Iv(i) => Some(*i as f64),
        SvLit::Uv(u) => Some(*u as f64),
        SvLit::Nv(n) => Some(*n),
        SvLit::Pv(s) => {
            // perl's leading-number rule, simplified
            let t = s.trim_start();
            let end = t
                .char_indices()
                .take_while(|(i, c)| {
                    c.is_ascii_digit() || *c == '.' || (*i == 0 && (*c == '-' || *c == '+'))
                })
                .map(|(i, c)| i + c.len_utf8())
                .last()
                .unwrap_or(0);
            Some(t[..end].parse::<f64>().unwrap_or(0.0))
        }
        _ => None,
    }
}

fn lit_display(lit: &SvLit) -> String {
    match lit {
        SvLit::Undef => "undef".into(),
        SvLit::Iv(i) => i.to_string(),
        SvLit::Uv(u) => u.to_string(),
        SvLit::Nv(n) => n.to_string(),
        SvLit::Pv(s) => format!("\"{}\"", crate::render::escape_str(s, false)),
        SvLit::RefTo(inner) => format!("\\{}", lit_display(inner)),
        SvLit::Code => "sub {...}".into(),
        SvLit::Glob { name, stash } => match stash {
            Some(s) => format!("*{}::{}", s, name),
            None => format!("*{}", name),
        },
        SvLit::Other(s) => format!("<{}>", s),
    }
}
