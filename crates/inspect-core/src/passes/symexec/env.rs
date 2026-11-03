//! Executor state (frames, pads, globals) and the operations on list
//! values and join points.

use std::collections::HashMap;

use super::value::{Cond, DefaultWhen, HashSeg, Key, Sym};

/// The pads and `@_` of one sub activation.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// Pad values by pad index; an unset entry is undef / empty.
    pub pads: HashMap<u64, Sym>,
    /// The current `@_` as a list value (`ArgRest(0)` on entry to the
    /// analyzed sub).
    pub args: Sym,
    pub callee: String,
}

/// The whole executor state on one path.
#[derive(Debug, Clone, PartialEq)]
pub struct Env {
    pub frames: Vec<Frame>,
    /// Package variables by sigiled qualified name (`$main::_`,
    /// `@DB::args`).
    pub globals: HashMap<String, Sym>,
    /// The branch conditions the path is under.
    pub conds: Vec<Cond>,
}

impl Env {
    pub fn new(callee: &str) -> Env {
        Env {
            frames: vec![Frame {
                pads: HashMap::new(),
                args: Sym::ArgRest(0),
                callee: callee.to_string(),
            }],
            globals: HashMap::new(),
            conds: Vec::new(),
        }
    }

    pub fn top(&self) -> &Frame {
        self.frames.last().expect("at least one frame")
    }

    pub fn top_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("at least one frame")
    }

    /// Merge the states of two paths that meet again. `ca` / `cb` are the
    /// conditions that led to each path (the alternatives of a merged
    /// value carry them).
    pub fn join(a: Env, b: Env, ca: &[Cond], cb: &[Cond], width: u32) -> Env {
        let conds = common_prefix(&a.conds, &b.conds);
        let mut frames = Vec::new();
        for (fa, fb) in a.frames.into_iter().zip(b.frames) {
            let mut pads = HashMap::new();
            let keys: Vec<u64> = fa.pads.keys().chain(fb.pads.keys()).copied().collect();
            for k in keys {
                if pads.contains_key(&k) {
                    continue;
                }
                let va = fa.pads.get(&k).cloned().unwrap_or_else(Sym::undef);
                let vb = fb.pads.get(&k).cloned().unwrap_or_else(Sym::undef);
                pads.insert(k, join_syms(ca, va, cb, vb, width));
            }
            let args = join_args(ca, fa.args, cb, fb.args, width);
            frames.push(Frame {
                pads,
                args,
                callee: fa.callee,
            });
        }
        let mut globals = HashMap::new();
        let gkeys: Vec<String> = a.globals.keys().chain(b.globals.keys()).cloned().collect();
        for k in gkeys {
            if globals.contains_key(&k) {
                continue;
            }
            let va = a.globals.get(&k).cloned().unwrap_or(Sym::Unknown("global"));
            let vb = b.globals.get(&k).cloned().unwrap_or(Sym::Unknown("global"));
            globals.insert(k, join_syms(ca, va, cb, vb, width));
        }
        Env {
            frames,
            globals,
            conds,
        }
    }
}

fn common_prefix(a: &[Cond], b: &[Cond]) -> Vec<Cond> {
    a.iter()
        .zip(b)
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.clone())
        .collect()
}

/// `@_` at a join point. A list known to be empty (the false arm of
/// `if (@_)`) also stands for the rest of the arguments after any number
/// of further shifts, so it merges into the other path's rest.
fn join_args(ca: &[Cond], a: Sym, cb: &[Cond], b: Sym, width: u32) -> Sym {
    if a == Sym::empty() && b.as_arg_slice().is_some() {
        return b;
    }
    if b == Sym::empty() && a.as_arg_slice().is_some() {
        return a;
    }
    join_syms(ca, a, cb, b, width)
}

/// Merge two values. Equal values stay as they are; otherwise the result
/// is a [`Sym::Join`] of the alternatives (flattened, deduplicated,
/// `Unknown("join")` beyond `width`), normalized to a [`Sym::Default`]
/// where it is the shape of a default.
pub fn join_syms(ca: &[Cond], a: Sym, cb: &[Cond], b: Sym, width: u32) -> Sym {
    if a == b {
        return a;
    }
    let mut alts: Vec<(Vec<Cond>, Sym)> = Vec::new();
    for (conds, s) in [(ca, a), (cb, b)] {
        match s {
            Sym::Join(inner) => {
                for (ic, is) in inner {
                    let mut c = conds.to_vec();
                    c.extend(ic);
                    push_alt(&mut alts, c, is);
                }
            }
            s => push_alt(&mut alts, conds.to_vec(), s),
        }
    }
    if alts.len() == 1 {
        return alts.pop().unwrap().1;
    }
    if alts.len() > width as usize {
        return Sym::Unknown("join");
    }
    normalize_default(&alts).unwrap_or(Sym::Join(alts))
}

fn push_alt(alts: &mut Vec<(Vec<Cond>, Sym)>, conds: Vec<Cond>, s: Sym) {
    if let Some((c, _)) = alts.iter_mut().find(|(_, x)| *x == s) {
        // the value holds under either condition set: keep what they share
        let shared = common_prefix(c, &conds);
        *c = shared;
    } else {
        alts.push((conds, s));
    }
}

/// `{c: x, !c: d}` where `c` tests `x` is a default for `x`:
/// `defined x` → `//`, `x` itself → `||`, `scalar(@_[n..])` with `x =
/// $_[n]` → missing argument.
fn normalize_default(alts: &[(Vec<Cond>, Sym)]) -> Option<Sym> {
    let [(c1, v1), (c2, v2)] = alts else {
        return None;
    };
    let ([k1], [k2]) = (c1.as_slice(), c2.as_slice()) else {
        return None;
    };
    if k1.sym != k2.sym || k1.truth == k2.truth {
        return None;
    }
    let (vt, vf) = if k1.truth { (v1, v2) } else { (v2, v1) };
    let when = match &k1.sym {
        Sym::App("defined", x) if x.len() == 1 && x[0] == *vt => DefaultWhen::Undef,
        // `exists $h{k} ? (value of $h{k}) : d`
        Sym::App("exists", x) if x.len() == 1 && covers(vt, &x[0]) => DefaultWhen::Missing,
        Sym::Len(l) if l.as_arg_slice().is_some_and(|(f, _)| *vt == Sym::Arg(f)) => {
            DefaultWhen::Missing
        }
        c if c == vt => DefaultWhen::False,
        _ => return None,
    };
    Some(Sym::Default {
        arg: Box::new(vt.clone()),
        default: Box::new(vf.clone()),
        when,
    })
}

/// Whether `v` may be `x` itself (every alternative of `x` is among those
/// of `v`).
fn covers(v: &Sym, x: &Sym) -> bool {
    fn alts(s: &Sym) -> Vec<&Sym> {
        match s {
            Sym::Join(a) => a.iter().map(|(_, s)| s).collect(),
            s => vec![s],
        }
    }
    let vs = alts(v);
    v == x || alts(x).iter().all(|a| vs.contains(a))
}

// ---- list values ----

/// Build a list from leading items and an optional unknown-length tail.
pub fn mk_list(items: Vec<Sym>, tail: Option<Sym>) -> Sym {
    match (items.is_empty(), tail) {
        (true, Some(t)) => t,
        (_, tail) => Sym::List {
            items,
            tail: tail.map(Box::new),
        },
    }
}

/// Leading items and tail of a list value.
pub fn view(l: &Sym) -> (Vec<Sym>, Option<Sym>) {
    match l {
        Sym::List { items, tail } => (items.clone(), tail.as_deref().cloned()),
        Sym::Hash { segs } if segs.iter().all(|s| matches!(s, HashSeg::Pairs(_))) => {
            let mut items = Vec::new();
            for s in segs {
                if let HashSeg::Pairs(p) = s {
                    for (k, v) in p {
                        items.push(Sym::pv(k));
                        items.push(v.clone());
                    }
                }
            }
            (items, None)
        }
        Sym::Const(_) | Sym::Ref(_) | Sym::Code(_) | Sym::RefPad { .. } | Sym::Alias { .. } => {
            (vec![l.clone()], None)
        }
        Sym::Join(alts) => {
            // alternatives that are lists of one length: a list of joins
            let views: Vec<(&Vec<Cond>, Vec<Sym>, Option<Sym>)> = alts
                .iter()
                .map(|(c, s)| match view(s) {
                    // a call that was not executed: taken as one value
                    // next to alternatives of one value
                    (i, Some(t @ Sym::Call { .. })) if i.is_empty() => (c, vec![t], None),
                    (i, t) => (c, i, t),
                })
                .collect();
            // (a shorter list reads as undef at the missing positions)
            let n = views.iter().map(|v| v.1.len()).max().unwrap_or(0);
            if views.iter().all(|(_, _, t)| t.is_none()) {
                let items = (0..n)
                    .map(|k| {
                        join_of(
                            views
                                .iter()
                                .map(|(c, i, _)| ((*c).clone(), i.get(k).cloned().unwrap_or_else(Sym::undef)))
                                .collect(),
                        )
                    })
                    .collect();
                (items, None)
            } else {
                (Vec::new(), Some(l.clone()))
            }
        }
        other => (Vec::new(), Some(other.clone())),
    }
}

/// Map a list operation over the alternatives of a joined list.
fn over_join<T>(l: &Sym, f: impl Fn(&Sym) -> T, merge: impl Fn(Vec<(Vec<Cond>, T)>) -> T) -> T {
    match l {
        Sym::Join(alts) => merge(alts.iter().map(|(c, s)| (c.clone(), f(s))).collect()),
        _ => f(l),
    }
}

fn join_of(alts: Vec<(Vec<Cond>, Sym)>) -> Sym {
    let mut out: Vec<(Vec<Cond>, Sym)> = Vec::new();
    for (c, s) in alts {
        push_alt(&mut out, c, s);
    }
    if out.len() == 1 {
        out.pop().unwrap().1
    } else {
        normalize_default(&out).unwrap_or(Sym::Join(out))
    }
}

fn join_pairs(alts: Vec<(Vec<Cond>, (Sym, Sym))>) -> (Sym, Sym) {
    let (a, b): (Vec<_>, Vec<_>) = alts
        .into_iter()
        .map(|(c, (x, y))| ((c.clone(), x), (c, y)))
        .unzip();
    (join_of(a), join_of(b))
}

/// `shift` of a list: (removed value, remaining list).
pub fn list_shift(l: &Sym) -> (Sym, Sym) {
    over_join(
        l,
        |l| {
            let (mut items, tail) = view(l);
            if !items.is_empty() {
                let first = items.remove(0);
                return (first, mk_list(items, tail));
            }
            match tail {
                None => (Sym::undef(), Sym::empty()),
                Some(t) => match t.as_arg_slice() {
                    Some((f, b)) => (Sym::Arg(f), Sym::arg_slice(f + 1, b)),
                    None => (Sym::Elem(Box::new(t), Key::Index(0)), Sym::Unknown("shift")),
                },
            }
        },
        join_pairs,
    )
}

/// `pop` of a list: (removed value, remaining list).
pub fn list_pop(l: &Sym) -> (Sym, Sym) {
    over_join(
        l,
        |l| {
            let (mut items, tail) = view(l);
            match tail {
                Some(t) => match t.as_arg_slice() {
                    Some((f, b)) => (Sym::ArgBack(b), mk_list(items, Some(Sym::arg_slice(f, b + 1)))),
                    None => (Sym::Elem(Box::new(t), Key::Index(-1)), Sym::Unknown("pop")),
                },
                None => match items.pop() {
                    Some(last) => (last, mk_list(items, None)),
                    None => (Sym::undef(), Sym::empty()),
                },
            }
        },
        join_pairs,
    )
}

/// Element `i` of a list (negative counts from the end).
pub fn list_elem(l: &Sym, i: i64) -> Sym {
    over_join(
        l,
        |l| {
            let (items, tail) = view(l);
            let n = items.len() as i64;
            if i >= 0 {
                if i < n {
                    return items[i as usize].clone();
                }
                match tail {
                    None => Sym::undef(),
                    Some(t) => match t.as_arg_slice() {
                        Some((f, _)) => Sym::Arg(f + (i - n) as u32),
                        None => Sym::Elem(Box::new(t), Key::Index(i - n)),
                    },
                }
            } else {
                match tail {
                    None if n + i >= 0 => items[(n + i) as usize].clone(),
                    None => Sym::undef(),
                    Some(t) => match t.as_arg_slice() {
                        Some((_, b)) => Sym::ArgBack(b + (-i - 1) as u32),
                        None => Sym::Elem(Box::new(l.clone()), Key::Index(i)),
                    },
                }
            }
        },
        join_of,
    )
}

/// The list without its first `n` items (the slurpy target of an
/// `aassign`).
pub fn list_drop(l: &Sym, n: usize) -> Sym {
    over_join(
        l,
        |l| {
            let (items, tail) = view(l);
            if items.len() >= n {
                return mk_list(items[n..].to_vec(), tail);
            }
            let skip = (n - items.len()) as u32;
            match tail {
                None => Sym::empty(),
                Some(t) => match t.as_arg_slice() {
                    Some((f, b)) => Sym::arg_slice(f + skip, b),
                    None => Sym::Unknown("list"),
                },
            }
        },
        join_of,
    )
}

/// Number of elements of a list.
pub fn list_len(l: &Sym) -> Sym {
    let (items, tail) = match l {
        Sym::Join(_) => return Sym::Len(Box::new(l.clone())),
        l => view(l),
    };
    match tail {
        None => Sym::iv(items.len() as i64),
        Some(_) => Sym::Len(Box::new(l.clone())),
    }
}

/// Concatenate list values (list context flattening).
pub fn list_concat(parts: Vec<Sym>) -> Sym {
    let mut items = Vec::new();
    let mut tail: Option<Sym> = None;
    for p in parts {
        if tail.is_some() {
            // a second unknown-length part: keep only that something follows
            tail = Some(Sym::Unknown("list"));
            continue;
        }
        let (pi, pt) = view(&p);
        items.extend(pi);
        tail = pt;
    }
    mk_list(items, tail)
}

/// A list value seen as a hash (`%h = LIST`).
pub fn to_hash(l: &Sym) -> Sym {
    match l {
        Sym::Hash { .. } => l.clone(),
        Sym::Join(alts) => Sym::Join(alts.iter().map(|(c, s)| (c.clone(), to_hash(s))).collect()),
        _ => {
            let (items, tail) = view(l);
            let mut segs = Vec::new();
            let mut pairs = Vec::new();
            let mut it = items.into_iter();
            let mut ok = true;
            while let Some(k) = it.next() {
                let v = it.next().unwrap_or_else(Sym::undef);
                match k.const_str() {
                    Some(k) => pairs.push((k, v)),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                return Sym::Hash {
                    segs: vec![HashSeg::Rest(l.clone())],
                };
            }
            if !pairs.is_empty() {
                segs.push(HashSeg::Pairs(pairs));
            }
            if let Some(t) = tail {
                segs.push(HashSeg::Rest(t));
            }
            Sym::Hash { segs }
        }
    }
}

/// Look a key up in a hash value. `on_default` is told about defaults
/// that come from pairs before a flattened list (`(k => d, @_)`).
pub fn hash_get(h: &Sym, key: &Key, on_default: &mut dyn FnMut(&Sym, &Sym)) -> Sym {
    match h {
        Sym::Hash { segs } => lookup_segs(segs, key, on_default),
        Sym::Ref(inner) => hash_get(inner, key, on_default),
        Sym::Join(alts) => join_of(
            alts.iter()
                .map(|(c, s)| (c.clone(), hash_get(s, key, on_default)))
                .collect(),
        ),
        Sym::Unknown(_) => h.clone(),
        other => Sym::Elem(Box::new(Sym::Deref(Box::new(other.clone()), '%')), key.clone()),
    }
}

fn lookup_segs(segs: &[HashSeg], key: &Key, on_default: &mut dyn FnMut(&Sym, &Sym)) -> Sym {
    let Some((last, before)) = segs.split_last() else {
        return Sym::undef();
    };
    match last {
        HashSeg::Pairs(p) => {
            if let Key::Str(k) = key {
                if let Some((_, v)) = p.iter().rev().find(|(pk, _)| pk == k) {
                    return v.clone();
                }
                lookup_segs(before, key, on_default)
            } else {
                Sym::Elem(
                    Box::new(Sym::Hash {
                        segs: segs.to_vec(),
                    }),
                    key.clone(),
                )
            }
        }
        HashSeg::Rest(r) => {
            let here = Sym::Elem(
                Box::new(Sym::Hash {
                    segs: vec![HashSeg::Rest(r.clone())],
                }),
                key.clone(),
            );
            if before.is_empty() {
                return here;
            }
            let earlier = lookup_segs(before, key, on_default);
            if earlier == Sym::undef() {
                return here;
            }
            on_default(&here, &earlier);
            Sym::Default {
                arg: Box::new(here),
                default: Box::new(earlier),
                when: DefaultWhen::Missing,
            }
        }
    }
}

/// Offset of the original argument count from the length of `l`:
/// `scalar(@_ original) = scalar(l) + offset` (None if `l` is not a rest
/// of the arguments).
pub fn arity_offset(l: &Sym) -> Option<i64> {
    let (items, tail) = view(l);
    let (f, b) = tail?.as_arg_slice()?;
    Some(f as i64 + b as i64 - items.len() as i64)
}
