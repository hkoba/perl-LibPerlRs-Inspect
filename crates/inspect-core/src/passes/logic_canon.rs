//! Identity of atoms for the logic pass ([`super::logic`]).
//!
//! Two sites denote the same atom when their *canonical keys* are equal.
//! A key names the variables an expression reads together with their
//! *generation*: the number of modifications of that variable seen so far
//! in walk order. So `defined($x)` before and after `$x = g()` are two
//! atoms, while `defined(my $x = f())` and a later `defined($x)` are one
//! (the value of an assignment is its target). Comparisons are brought to
//! a canonical operator (`<`, `==`, `lt`, `eq`) so that `a >= b` is the
//! negation of `a < b` and `b > a` is `a < b`.
//!
//! What counts as a modification is decided structurally from the op
//! tree, see [`modified_vars`]. Perl marks plain sub-call arguments
//! (`f($x)`, `$x->m`) as `OPf_MOD` because `@_` aliases them; by default
//! that is not treated as a modification (`WalkOpts::alias_modifies`).
//!
//! That is [`Identity::Strict`]. Under [`Identity::Expr`] and
//! [`Identity::Var`] generations are never bumped (every variable stays
//! at generation 0), and an assignment is keyed by its text (`Expr`) or
//! by its target (`Var`); see [`value_key`] and [`display`].

use std::collections::HashMap;

use crate::ir::{OpDetail, OpNode, SubIr};
use crate::render::render;

use super::logic::{Identity, WalkOpts};

const OPF_REF: u8 = 16;
const OPF_MOD: u8 = 32;
const OPF_STACKED: u8 = 64;
const OPP_LVAL_INTRO: u8 = 128;
const OPP_TARGET_MY: u8 = 16;
const OPP_MD_DELETE: u8 = 32;
const OPP_LVAL_DEFER: u8 = 64;
const OPP_PADRANGE_COUNTMASK: u8 = 0x7f;

/// A variable as an identity: lexicals by pad index, package variables by
/// sigil and fully qualified name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum VarId {
    Pad(u64),
    Gv { sigil: char, name: String },
}

impl VarId {
    fn gv(sigil: char, name: &str, stash: Option<&str>) -> VarId {
        let name = if name.contains("::") {
            name.to_string()
        } else {
            format!("{}::{}", stash.unwrap_or("main"), name)
        };
        VarId::Gv { sigil, name }
    }
}

/// Canonical identity of an atom at one site. Equal `key`s are one
/// column; `neg` says the site's truth is the key's negation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Canon {
    pub key: String,
    pub neg: bool,
}

/// Generations of the variables modified so far.
#[derive(Debug, Clone)]
pub(crate) struct Env {
    gens: HashMap<VarId, u32>,
    /// Whether modifications count ([`Identity::tracks_generations`]).
    track: bool,
}

impl Default for Env {
    fn default() -> Self {
        Env::new(true)
    }
}

impl Env {
    pub(crate) fn new(track: bool) -> Env {
        Env { gens: HashMap::new(), track }
    }

    pub(crate) fn bump(&mut self, id: VarId) {
        if self.track {
            *self.gens.entry(id).or_insert(0) += 1;
        }
    }

    /// Bump each distinct variable once (a scan may report the same
    /// modification through several rules).
    pub(crate) fn bump_all(&mut self, ids: Vec<VarId>) {
        let mut seen: Vec<VarId> = Vec::new();
        for id in ids {
            if !seen.contains(&id) {
                self.bump(id.clone());
                seen.push(id);
            }
        }
    }

    fn generation(&self, id: &VarId) -> u32 {
        self.gens.get(id).copied().unwrap_or(0)
    }

    /// After a branch: every variable is at the newest generation of
    /// either arm, so atoms after the branch never merge with atoms before
    /// a modification in any arm.
    pub(crate) fn join(&mut self, other: &Env) {
        for (k, v) in &other.gens {
            let e = self.gens.entry(k.clone()).or_insert(0);
            *e = (*e).max(*v);
        }
    }
}

fn real_kids(n: &OpNode) -> Vec<&OpNode> {
    n.kids
        .iter()
        .map(|k| k.skip_null())
        .filter(|k| k.name != "pushmark")
        .collect()
}

/// The variable a leaf denotes.
fn var_of(n: &OpNode) -> Option<VarId> {
    let n = n.skip_null();
    match n.name.as_str() {
        "padsv" | "padav" | "padhv" => Some(VarId::Pad(n.targ)),
        "gvsv" => match &n.detail {
            OpDetail::Gv { name, stash } => Some(VarId::gv('$', name, stash.as_deref())),
            _ => None,
        },
        "rv2sv" | "rv2av" | "rv2hv" | "rv2gv" => {
            let sigil = match n.name.as_str() {
                "rv2sv" => '$',
                "rv2av" => '@',
                "rv2hv" => '%',
                _ => '*',
            };
            match n.kids.first().map(|k| k.skip_null()) {
                Some(g) if g.name == "gv" => match &g.detail {
                    OpDetail::Gv { name, stash } => Some(VarId::gv(sigil, name, stash.as_deref())),
                    _ => None,
                },
                _ => None,
            }
        }
        "gv" => match &n.detail {
            OpDetail::Gv { name, stash } => Some(VarId::gv('$', name, stash.as_deref())),
            _ => None,
        },
        _ => None,
    }
}

/// The variable at the root of an element / slice / dereference chain:
/// `$h{k}`, `$r->{x}[0]`, `@$r`, `${$x}` all come down to one variable.
fn root_var(n: &OpNode) -> Option<VarId> {
    let n = n.skip_null();
    match n.name.as_str() {
        "helem" | "aelem" | "aslice" | "hslice" | "kvaslice" | "kvhslice" | "delete" | "exists"
        | "rv2sv" | "rv2av" | "rv2hv" | "rv2gv" | "srefgen" | "refgen" | "list" => {
            if let Some(v) = var_of(n) {
                return Some(v);
            }
            real_kids(n).first().and_then(|k| root_var(k))
        }
        "multideref" => match &n.detail {
            OpDetail::MultiDeref { steps } => {
                let s = steps.first()?;
                match s.base.as_str() {
                    "padsv" | "padav" | "padhv" => s.base_targ.map(VarId::Pad),
                    "gvsv" => s.base_name.as_deref().map(|nm| VarId::gv('$', nm, None)),
                    "gvav" => s.base_name.as_deref().map(|nm| VarId::gv('@', nm, None)),
                    "gvhv" => s.base_name.as_deref().map(|nm| VarId::gv('%', nm, None)),
                    _ => real_kids(n).first().and_then(|k| root_var(k)),
                }
            }
            _ => None,
        },
        _ => var_of(n),
    }
}

/// Ops that can carry `OPpTARGET_MY` (`$x = $a + $b` stores straight into
/// the lexical). The bit means something else on other ops.
const TARGLEX_OPS: &[&str] = &[
    "add", "subtract", "multiply", "divide", "modulo", "pow", "repeat", "concat", "multiconcat",
    "stringify", "left_shift", "right_shift", "bit_and", "bit_or", "bit_xor", "negate",
    "complement", "i_add", "i_subtract", "i_multiply", "i_divide", "i_modulo", "i_negate",
    "nbit_and", "nbit_or", "nbit_xor", "sbit_and", "sbit_or", "sbit_xor", "atan2", "sin",
    "cos", "exp", "log", "sqrt", "int", "hex", "oct", "abs", "length", "index", "rindex",
    "sprintf", "ord", "chr", "crypt", "ucfirst", "lcfirst", "uc", "lc", "quotemeta", "join",
    "undef", "time", "rand", "srand", "vec", "substr", "emptyavhv",
];

/// Every variable the ops under `node` modify, in pre-order (a variable
/// may be listed more than once). Assignments, `my` / `local`, `++`,
/// `.=` and friends, `chomp`, `s///`, `push`/`shift`/..., element and
/// slice lvalues (reported as their root variable), `foreach` variables
/// and `\$x` (a reference may be written through later) count; plain
/// sub-call arguments only with `opts.alias_modifies`.
pub(crate) fn modified_vars(node: &OpNode, opts: &WalkOpts, out: &mut Vec<VarId>) {
    let n = node;
    let kids = real_kids(n);
    let lvalue = |k: &OpNode, out: &mut Vec<VarId>| {
        if let Some(v) = root_var(k) {
            out.push(v);
        }
    };
    match n.name.as_str() {
        "padsv" | "padav" | "padhv" => {
            let intro = n.private & OPP_LVAL_INTRO != 0;
            let modref = n.flags & OPF_MOD != 0 && n.flags & OPF_REF != 0;
            let aliased = opts.alias_modifies && n.flags & OPF_MOD != 0;
            if intro || modref || aliased {
                out.push(VarId::Pad(n.targ));
            }
        }
        "padrange" => {
            if n.private & OPP_LVAL_INTRO != 0 || n.flags & OPF_MOD != 0 {
                let count = u64::from(n.private & OPP_PADRANGE_COUNTMASK);
                for i in 0..count {
                    out.push(VarId::Pad(n.targ + i));
                }
            }
        }
        "padsv_store" => out.push(VarId::Pad(n.targ)),
        "sassign" => {
            if let Some(lhs) = n.kids.get(1) {
                lvalue(lhs, out);
            }
        }
        "aassign" => {
            if let Some(lhs) = n.kids.get(1) {
                for k in real_kids(lhs.skip_null()) {
                    if k.name == "padrange" {
                        continue; // its padsv kids are still in the tree
                    }
                    lvalue(k, out);
                }
            }
        }
        "orassign" | "andassign" | "dorassign" | "preinc" | "predec" | "postinc" | "postdec"
        | "i_preinc" | "i_predec" | "i_postinc" | "i_postdec" => {
            if let Some(k) = kids.first() {
                lvalue(k, out);
            }
        }
        "chomp" | "chop" | "schomp" | "schop" => {
            for k in &kids {
                lvalue(k, out);
            }
        }
        "undef" => {
            if let Some(k) = kids.first() {
                lvalue(k, out);
            }
        }
        "multiconcat" | "subst" | "trans" | "transr" => {
            if n.flags & OPF_STACKED != 0 {
                if let Some(k) = kids.first() {
                    lvalue(k, out);
                }
            }
            let targ_my = n.name != "multiconcat" || n.private & OPP_TARGET_MY != 0;
            if n.targ != 0 && targ_my {
                out.push(VarId::Pad(n.targ));
            }
        }
        "push" | "unshift" | "pop" | "shift" | "splice" => {
            if let Some(k) = kids.first() {
                lvalue(k, out);
            }
        }
        "read" | "sysread" | "recv" => {
            if let Some(k) = kids.get(1) {
                lvalue(k, out);
            }
        }
        "gvsv" | "rv2sv" | "rv2av" | "rv2hv" => {
            // `local`
            if n.private & OPP_LVAL_INTRO != 0 {
                if let Some(v) = var_of(n) {
                    out.push(v);
                }
            }
        }
        "srefgen" | "refgen" => {
            for k in &kids {
                lvalue(k, out);
            }
        }
        "enteriter" => {
            if n.targ != 0 {
                out.push(VarId::Pad(n.targ));
            } else {
                // `for $g (...)` / `for (...)`: the last kid names the
                // iterator (rv2gv > gv, gv `*_`, or a padsv)
                if let Some(k) = kids.last() {
                    if let Some(v) = var_of(k) {
                        // `for $g (...)` names the glob; the scalar is iterated
                        out.push(match v {
                            VarId::Gv { sigil: '*', name } => VarId::Gv { sigil: '$', name },
                            v => v,
                        });
                    }
                }
            }
        }
        "multideref" => {
            let defer = n.private & OPP_LVAL_DEFER != 0;
            let modified = n.private & OPP_MD_DELETE != 0
                || (n.flags & OPF_MOD != 0 && (!defer || opts.alias_modifies));
            if modified {
                lvalue(n, out);
            }
        }
        "helem" | "aelem" => {
            let defer = n.private & OPP_LVAL_DEFER != 0;
            if n.flags & OPF_MOD != 0 && (!defer || opts.alias_modifies) {
                lvalue(n, out);
            }
        }
        "delete" => lvalue(n, out),
        "substr" => {
            if n.flags & OPF_MOD != 0 {
                if let Some(k) = kids.first() {
                    lvalue(k, out);
                }
            }
        }
        _ => {}
    }
    if n.private & OPP_TARGET_MY != 0 && n.targ != 0 && TARGLEX_OPS.contains(&n.name.as_str()) {
        out.push(VarId::Pad(n.targ));
    }
    for k in &n.kids {
        modified_vars(k, opts, out);
    }
}

/// `$x#5@2` (name, pad index, generation) / `$main::g@1`.
fn var_key(ir: &SubIr, env: &Env, id: &VarId) -> String {
    match id {
        VarId::Pad(t) => format!(
            "{}#{}@{}",
            ir.pad_name(*t).unwrap_or("?"),
            t,
            env.generation(id)
        ),
        VarId::Gv { sigil, name } => format!("{sigil}{name}@{}", env.generation(id)),
    }
}

/// The variables an expression reads, in pre-order (element chains report
/// their root variable).
fn read_vars(n: &OpNode, out: &mut Vec<VarId>) {
    if let Some(v) = var_of(n).or_else(|| (n.name == "multideref").then(|| root_var(n)).flatten()) {
        out.push(v);
    }
    for k in &n.kids {
        read_vars(k, out);
    }
}

/// Identity of the value of `node`, applying the modifications it makes.
/// An assignment is its target (`Strict`, `Var`) or its own text (`Expr`).
pub(crate) fn value_key(ir: &SubIr, env: &mut Env, opts: &WalkOpts, node: &OpNode) -> String {
    let n = node.skip_null();
    if let Some(v) = var_of(n) {
        return var_key(ir, env, &v);
    }
    match (n.name.as_str(), opts.identity) {
        (_, Identity::Expr) => {}
        // The value of an assignment is its target.
        ("sassign", _) => {
            if let (Some(rhs), Some(lhs)) = (n.kids.first(), n.kids.get(1)) {
                if let Some(v) = var_of(lhs) {
                    let mut mods = Vec::new();
                    modified_vars(rhs, opts, &mut mods);
                    env.bump_all(mods);
                    env.bump(v.clone());
                    return var_key(ir, env, &v);
                }
            }
        }
        ("padsv_store", _) => {
            let v = VarId::Pad(n.targ);
            let mut mods = Vec::new();
            for k in &n.kids {
                modified_vars(k, opts, &mut mods);
            }
            env.bump_all(mods);
            env.bump(v.clone());
            return var_key(ir, env, &v);
        }
        _ => {}
    }
    // Opaque: the rendered text plus the generation of every variable it
    // reads. An expression that itself modifies a variable (`$x++ > 1`)
    // may read the old or the new value, so it is made unique — when
    // generations are tracked at all.
    let mut mods = Vec::new();
    modified_vars(n, opts, &mut mods);
    let mut reads = Vec::new();
    read_vars(n, &mut reads);
    let mut key = render(ir, n);
    if !reads.is_empty() {
        let gens: Vec<String> = reads.iter().map(|v| var_key(ir, env, v)).collect();
        key.push_str(&format!(" ⟨{}⟩", gens.join(",")));
    }
    if !mods.is_empty() {
        if opts.identity.tracks_generations() {
            key.push_str(&format!("#node{}", n.id));
        }
        env.bump_all(mods);
    }
    key
}

/// The text an atom is shown as: its rendering, with every assignment to
/// a variable collapsed to the variable under [`Identity::Var`]
/// (`defined($x = f())` → `defined($x)`).
pub(crate) fn display(ir: &SubIr, opts: &WalkOpts, node: &OpNode) -> String {
    if opts.identity != Identity::Var {
        return render(ir, node);
    }
    render(ir, &collapse_assigns(node))
}

fn collapse_assigns(node: &OpNode) -> OpNode {
    let n = node.skip_null();
    match n.name.as_str() {
        "sassign" => {
            if let Some(lhs) = n.kids.get(1) {
                if var_of(lhs).is_some() {
                    return lhs.clone();
                }
            }
        }
        "padsv_store" => {
            let mut v = n.clone();
            v.name = "padsv".into();
            v.kids.clear();
            return v;
        }
        _ => {}
    }
    let mut out = n.clone();
    out.kids = n.kids.iter().map(collapse_assigns).collect();
    out
}

/// Canonical identity of the atom `node`.
pub(crate) fn canon(ir: &SubIr, env: &mut Env, opts: &WalkOpts, node: &OpNode) -> Canon {
    let n = node.skip_null();
    let plain = |key: String| Canon { key, neg: false };
    match n.name.as_str() {
        "defined" => {
            if let Some(k) = n.kids.first() {
                return plain(format!("defined({})", value_key(ir, env, opts, k)));
            }
        }
        "lt" | "gt" | "le" | "ge" | "eq" | "ne" | "i_lt" | "i_gt" | "i_le" | "i_ge" | "i_eq"
        | "i_ne" | "slt" | "sgt" | "sle" | "sge" | "seq" | "sne" => {
            if let (Some(l), Some(r)) = (n.kids.first(), n.kids.get(1)) {
                let a = value_key(ir, env, opts, l);
                let b = value_key(ir, env, opts, r);
                let op = n.name.trim_start_matches("i_");
                let (prefix, op) = match op.strip_prefix('s') {
                    Some(rest) if rest.len() == 2 => ("s", rest),
                    _ => ("", op),
                };
                let (key, neg) = match op {
                    "lt" => (format!("{prefix}lt({a},{b})"), false),
                    "gt" => (format!("{prefix}lt({b},{a})"), false),
                    "ge" => (format!("{prefix}lt({a},{b})"), true),
                    "le" => (format!("{prefix}lt({b},{a})"), true),
                    _ => {
                        let (x, y) = if a <= b { (&a, &b) } else { (&b, &a) };
                        (format!("{prefix}eq({x},{y})"), op == "ne")
                    }
                };
                return Canon { key, neg };
            }
        }
        _ => {}
    }
    plain(value_key(ir, env, opts, node))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{OpClass, SvLit};

    fn node(id: u32, name: &str, kids: Vec<OpNode>) -> OpNode {
        OpNode {
            id,
            name: name.into(),
            op_type: 1,
            class: OpClass::Base,
            was: None,
            flags: 0,
            private: 0,
            targ: 0,
            next: None,
            other: None,
            detail: OpDetail::None,
            kids,
        }
    }

    fn pad(id: u32, targ: u64) -> OpNode {
        let mut n = node(id, "padsv", vec![]);
        n.targ = targ;
        n
    }

    fn konst(id: u32, iv: i64) -> OpNode {
        let mut n = node(id, "const", vec![]);
        n.detail = OpDetail::Const(SvLit::Iv(iv));
        n
    }

    fn ir_with(root: OpNode) -> SubIr {
        use crate::ir::PadEntry;
        SubIr {
            file: None,
            lines: None,
            proto: None,
            pad: vec![
                PadEntry { ix: 1, name: Some("$x".into()), typ: None, flags: 0, our_stash: None, line: None, scope: None },
                PadEntry { ix: 2, name: Some("$y".into()), typ: None, flags: 0, our_stash: None, line: None, scope: None },
            ],
            start_id: None,
            root,
            anon_subs: vec![],
        }
    }

    fn cmp(op: &str, l: OpNode, r: OpNode) -> OpNode {
        node(100, op, vec![l, r])
    }

    #[test]
    fn comparisons_flip_to_lt_and_eq() {
        let opts = WalkOpts::default();
        let ir = ir_with(node(0, "leavesub", vec![]));
        let mut env = Env::default();
        let lt = canon(&ir, &mut env, &opts, &cmp("lt", pad(1, 1), konst(2, 8)));
        let ge = canon(&ir, &mut env, &opts, &cmp("ge", pad(1, 1), konst(2, 8)));
        let gt_swapped = canon(&ir, &mut env, &opts, &cmp("gt", konst(2, 8), pad(1, 1)));
        let le_swapped = canon(&ir, &mut env, &opts, &cmp("le", konst(2, 8), pad(1, 1)));
        assert_eq!(lt.key, "lt($x#1@0,8)");
        assert!(!lt.neg);
        assert_eq!(ge, Canon { key: lt.key.clone(), neg: true });
        assert_eq!(gt_swapped, lt);
        assert_eq!(le_swapped, ge);
        let eq = canon(&ir, &mut env, &opts, &cmp("eq", pad(1, 1), pad(2, 2)));
        let ne = canon(&ir, &mut env, &opts, &cmp("ne", pad(2, 2), pad(1, 1)));
        assert_eq!(eq.key, "eq($x#1@0,$y#2@0)");
        assert_eq!(ne, Canon { key: eq.key, neg: true });
        let seq = canon(&ir, &mut env, &opts, &cmp("sne", pad(1, 1), konst(2, 1)));
        assert!(seq.key.starts_with("seq(") && seq.neg);
    }

    #[test]
    fn assignment_value_is_its_target() {
        let opts = WalkOpts::default();
        let ir = ir_with(node(0, "leavesub", vec![]));
        let mut env = Env::default();
        let mut lhs = pad(3, 1);
        lhs.private = OPP_LVAL_INTRO;
        lhs.flags = OPF_MOD | OPF_REF;
        let assign = node(4, "sassign", vec![konst(5, 1), lhs]);
        let defined = node(6, "defined", vec![assign]);
        let c = canon(&ir, &mut env, &opts, &defined);
        assert_eq!(c.key, "defined($x#1@1)");
        let later = canon(&ir, &mut env, &opts, &node(7, "defined", vec![pad(8, 1)]));
        assert_eq!(later.key, c.key);
        env.bump(VarId::Pad(1));
        let after = canon(&ir, &mut env, &opts, &node(9, "defined", vec![pad(10, 1)]));
        assert_eq!(after.key, "defined($x#1@2)");
    }

    #[test]
    fn self_modifying_expression_is_unique() {
        let opts = WalkOpts::default();
        let ir = ir_with(node(0, "leavesub", vec![]));
        let mut env = Env::default();
        let mut target = pad(1, 1);
        target.flags = OPF_MOD | OPF_REF;
        let inc = node(2, "postinc", vec![target]);
        let c = canon(&ir, &mut env, &opts, &cmp("gt", inc, konst(3, 1)));
        assert!(c.key.contains("#node"), "{}", c.key);
        assert_eq!(env.generation(&VarId::Pad(1)), 1);
    }

    #[test]
    fn alias_only_counts_when_asked() {
        let mut arg = pad(1, 1);
        arg.flags = OPF_MOD;
        let call = node(2, "entersub", vec![arg]);
        let mut out = Vec::new();
        modified_vars(&call, &WalkOpts::default(), &mut out);
        assert!(out.is_empty());
        let opts = WalkOpts { alias_modifies: true, ..Default::default() };
        modified_vars(&call, &opts, &mut out);
        assert_eq!(out, [VarId::Pad(1)]);
    }

    fn assign(id: u32, targ: u64, rhs: OpNode) -> OpNode {
        node(id, "sassign", vec![rhs, pad(id + 1, targ)])
    }

    fn call(id: u32, name: &str) -> OpNode {
        // entersub > gv: opaque, reads no variable
        let mut g = node(id + 1, "gv", vec![]);
        g.detail = OpDetail::Gv { name: name.into(), stash: Some("main".into()) };
        node(id, "entersub", vec![g])
    }

    /// `defined($x = a())`, `$x >= 3`, `defined($x = b())`, `$x < 3`
    fn keys_under(identity: Identity) -> Vec<Canon> {
        let opts = WalkOpts { identity, ..Default::default() };
        let ir = ir_with(node(0, "leavesub", vec![]));
        let mut env = Env::new(identity.tracks_generations());
        [
            node(10, "defined", vec![assign(11, 1, call(12, "a"))]),
            cmp("ge", pad(20, 1), konst(21, 3)),
            node(30, "defined", vec![assign(31, 1, call(32, "b"))]),
            cmp("lt", pad(40, 1), konst(41, 3)),
        ]
        .iter()
        .map(|n| canon(&ir, &mut env, &opts, n))
        .collect()
    }

    fn distinct(keys: &[Canon]) -> usize {
        let mut seen: Vec<&str> = Vec::new();
        for k in keys {
            if !seen.contains(&k.key.as_str()) {
                seen.push(&k.key);
            }
        }
        seen.len()
    }

    #[test]
    fn strict_keeps_the_reassigned_tests_apart() {
        let k = keys_under(Identity::Strict);
        assert_eq!(distinct(&k), 4, "{k:?}");
        assert!(k[1].neg && !k[3].neg);
    }

    #[test]
    fn expr_keys_assignments_by_their_text() {
        let k = keys_under(Identity::Expr);
        assert_eq!(distinct(&k), 3, "{k:?}");
        assert_eq!(k[1].key, k[3].key, "$x >= 3 and $x < 3 are one column");
        assert_ne!(k[0].key, k[2].key, "different assignments");
    }

    #[test]
    fn var_keys_assignments_by_their_target() {
        let k = keys_under(Identity::Var);
        assert_eq!(distinct(&k), 2, "{k:?}");
        assert_eq!(k[0].key, k[2].key);
        assert_eq!(k[1].key, k[3].key);
    }

    #[test]
    fn var_display_collapses_assignments() {
        let ir = ir_with(node(0, "leavesub", vec![]));
        let n = node(10, "defined", vec![assign(11, 1, call(12, "a"))]);
        let var = WalkOpts { identity: Identity::Var, ..Default::default() };
        assert_eq!(display(&ir, &var, &n), "defined($x)");
        assert_eq!(display(&ir, &WalkOpts::default(), &n), "defined($x = a())");
        let mut store = node(13, "padsv_store", vec![call(14, "a")]);
        store.targ = 2;
        let n = cmp("lt", store, konst(15, 3));
        assert_eq!(display(&ir, &var, &n), "$y < 3");
    }

    #[test]
    fn env_join_takes_the_newest_generation() {
        let mut a = Env::default();
        let mut b = a.clone();
        a.bump(VarId::Pad(1));
        b.bump(VarId::Pad(2));
        b.bump(VarId::Pad(2));
        a.join(&b);
        assert_eq!(a.generation(&VarId::Pad(1)), 1);
        assert_eq!(a.generation(&VarId::Pad(2)), 2);
    }
}
