//! M3: structural path analysis of guard conditions (truth tables).
//!
//! The sub body is walked statement by statement. Every branch point
//! (`if` / `elsif` / `else`, `STMT if COND`, `unless`, the ternary
//! operator, `//`) and every boolean operator reachable from a decomposed
//! position contributes *atomic conditions*. Each reachable outcome
//! (`return`, `die`, `croak` / `confess`, or the implicit final expression)
//! is recorded together with the conjunction of literals that leads to it.
//! When the number of atoms is small (at most `MAX_TABLE_CONDS`) a full
//! truth table over all combinations is produced as well.
//!
//! Atom rules:
//! - operands of `&&` / `||` / `!` and the condition of `?:` are atoms,
//!   identified by their rendered text; `!X` flips the polarity of `X` and
//!   never creates an atom of its own;
//! - the left-hand side of `//` stays opaque: its atom is `defined(LHS)`.
//!   If an enclosing `&&` / `||` also tests the *value* of the LHS, a
//!   second atom `LHS` is interned on demand;
//! - the arms of `?:` in value position are values, not atoms
//!   (`$x ? "yes" : "no"` yields the single atom `$x`);
//! - `xor`, `&&=` / `||=` / `//=`, calls, comparisons and everything else
//!   are opaque leaves.
//!
//! Decomposed positions: guard conditions (`cond_expr` and statement-level
//! `and` / `or` / `dor`), the single expression of `return EXPR`, and the
//! implicit final expression, whenever the top op is `and` / `or` / `dor` /
//! `cond_expr` (or `not` applied to one of them). Assignments, call
//! arguments, `die` / `croak` messages, list returns and loop control
//! conditions are not decomposed.
//!
//! Every short-circuit route through an expression becomes a separate
//! path, so the same outcome may appear several times with different
//! `when` literals. Within a decomposed expression the routes come in
//! source order: the short-circuit route (left operand decides) before the
//! routes through the right operand, and the true arm of `?:` before the
//! false arm. For statement modifiers (`STMT if COND`) the outcomes of the
//! statement precede the fall-through outcome. `&&` / `||` yield the value
//! of the deciding operand, as in Perl: `return $a && $b` reports `$b`
//! when both are true.
//!
//! Remaining limitations:
//! - atoms are compared textually; semantic implication such as
//!   `$v > 10` => `$v > 5` is not considered, so a truth table may contain
//!   rows that are semantically unreachable;
//! - after a branch whose arms both fall through, the branch literals are
//!   dropped (only early-return shapes carry literals forward);
//! - when the continuation after an early return would fan out into more
//!   than `MAX_PATH_SET` conjunctions, those literals are dropped as well;
//! - `die` inside `eval BLOCK` is caught, but its kind is still reported
//!   as `die`;
//! - loop control conditions are not atoms; only branches in the loop body
//!   are collected;
//! - `render` does not track precedence, so an opaque `!($a && $b)` (for
//!   example as a `die` argument) is displayed as `!$a && $b`.

use serde::{Deserialize, Serialize};

use crate::ir::{OpClass, OpNode, SubIr};
use crate::render::render;

use super::retspec::{croak_call, return_exprs};

/// Above this many atoms no truth table (2^n rows) is generated.
pub const MAX_TABLE_CONDS: usize = 6;

/// Upper bound on the number of conjunctions carried past an early
/// return; beyond it the continuation literals are dropped.
const MAX_PATH_SET: usize = 1 << MAX_TABLE_CONDS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogicSpec {
    /// Rendered text of each atomic condition, in order of first
    /// appearance. `when` and `table` index into this list.
    pub conds: Vec<String>,
    /// Reachable outcomes with their path conditions (source order).
    pub paths: Vec<PathOutcome>,
    /// Truth table; `None` when there are no atoms or more than
    /// `MAX_TABLE_CONDS`.
    pub table: Option<Vec<TableRow>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathOutcome {
    pub line: Option<u32>,
    /// return | die | croak | confess | implicit
    pub kind: String,
    pub exprs: Vec<String>,
    /// Conjunction of literals under which this outcome is reached.
    pub when: Vec<CondLit>,
}

/// One literal of a path condition: `conds[cond]` has the value `value`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CondLit {
    pub cond: usize,
    pub value: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableRow {
    /// Truth assignment in `conds` order (`conds[0]` is the most
    /// significant bit of the enumeration).
    pub inputs: Vec<bool>,
    /// Index of the first path whose `when` holds under `inputs`
    /// (`None` when no path matches).
    pub path: Option<usize>,
}

pub fn analyze_logic(ir: &SubIr) -> LogicSpec {
    let mut w = Walker {
        ir,
        conds: Vec::new(),
        paths: Vec::new(),
    };
    let stmts = super::statements(ir);
    w.walk_block(&stmts, &vec![vec![]], true);
    let table = w.build_table();
    LogicSpec {
        conds: w.conds,
        paths: w.paths,
        table,
    }
}

/// A set of pairwise exclusive conjunctions. `vec![vec![]]` means
/// "unconditionally reachable"; an empty set means "unreachable".
type PathSet = Vec<Vec<CondLit>>;

/// One short-circuit route through a boolean expression.
#[derive(Debug, Clone)]
struct Route {
    /// Complete path condition (incoming literals plus those of the route).
    lits: Vec<CondLit>,
    /// Atom whose truth decides the expression on this route, if one has
    /// been interned (`None` for an opaque `//` left-hand side).
    atom: Option<usize>,
    /// Rendered value of the expression on this route (before negation).
    value: String,
    /// The value is negated an odd number of times by enclosing `not`s.
    neg: bool,
}

/// How a statement-level guard tests its condition.
#[derive(Clone, Copy)]
enum Test {
    /// Truthiness (`if` / `unless` / `&&` / `||`).
    Truth,
    /// Definedness (`//`): the condition itself stays opaque.
    Defined,
}

/// How the statements after the current one are reached.
enum Flow {
    /// Unchanged path set.
    Continue,
    /// Some routes terminated; the rest continue with this path set.
    ContinueWith(PathSet),
    /// Every route ended in return/die (the rest is unreachable).
    Terminates,
}

/// Extend a conjunction with a literal. Returns `None` when the literal
/// contradicts one already present, and the unchanged conjunction when it
/// is already there.
fn push_lit(lits: &[CondLit], cond: usize, value: bool) -> Option<Vec<CondLit>> {
    if let Some(l) = lits.iter().find(|l| l.cond == cond) {
        return (l.value == value).then(|| lits.to_vec());
    }
    let mut v = lits.to_vec();
    v.push(CondLit { cond, value });
    Some(v)
}

/// Skip `null` wrappers and the `lineseq(ex-nextstate, COND)` that
/// surrounds `elsif` conditions.
fn strip(node: &OpNode) -> &OpNode {
    let n = node.skip_null();
    if n.name == "lineseq" {
        if let Some(last) = n.kids.iter().filter(|k| k.class != OpClass::Cop).last() {
            return last.skip_null();
        }
    }
    n
}

/// A value-position expression is decomposed when its top op is a
/// short-circuit operator or a ternary (possibly under `not`).
fn is_decomposable(node: &OpNode) -> bool {
    fn top(n: &OpNode) -> bool {
        matches!(n.name.as_str(), "and" | "or" | "dor" | "cond_expr")
    }
    let n = strip(node);
    if top(n) {
        return true;
    }
    n.name == "not" && n.kids.first().is_some_and(|k| top(strip(k)))
}

fn value_str(r: &Route) -> String {
    if r.neg {
        format!("!{}", r.value)
    } else {
        r.value.clone()
    }
}

fn lits_of(routes: Vec<Route>) -> PathSet {
    routes.into_iter().map(|r| r.lits).collect()
}

struct Walker<'a> {
    ir: &'a SubIr,
    conds: Vec<String>,
    paths: Vec<PathOutcome>,
}

impl<'a> Walker<'a> {
    /// Walk a statement list. Returns whether every route terminated.
    /// `tail` says whether the list sits in tail position of the sub (or
    /// of an arm), so that its last expression is an implicit return.
    fn walk_block(
        &mut self,
        stmts: &[(Option<u32>, &'a OpNode)],
        path: &PathSet,
        tail: bool,
    ) -> bool {
        if path.is_empty() {
            return true;
        }
        let mut path = path.clone();
        for (i, (line, node)) in stmts.iter().enumerate() {
            let stmt_tail = tail && i + 1 == stmts.len();
            match self.walk_stmt(*line, node, &path, stmt_tail) {
                Flow::Terminates => return true,
                Flow::ContinueWith(set) => {
                    if set.is_empty() {
                        return true;
                    }
                    if set.len() <= MAX_PATH_SET {
                        path = set;
                    }
                }
                Flow::Continue => {}
            }
        }
        false
    }

    fn walk_stmt(
        &mut self,
        line: Option<u32>,
        node: &'a OpNode,
        path: &PathSet,
        tail: bool,
    ) -> Flow {
        let n = node.skip_null();
        match n.name.as_str() {
            "return" => {
                self.emit_return(line, n, path);
                Flow::Terminates
            }
            "die" => {
                let exprs = return_exprs(self.ir, n);
                self.emit(line, "die", exprs, path);
                Flow::Terminates
            }
            "entersub" => {
                if let Some((via, msg)) = croak_call(self.ir, n) {
                    self.emit(line, &via, msg.into_iter().collect(), path);
                    Flow::Terminates
                } else {
                    self.tail_implicit(line, node, path, tail);
                    Flow::Continue
                }
            }
            "cond_expr" => self.walk_cond_expr(line, n, path, tail),
            "and" | "or" | "dor" => self.walk_logop(line, n, path, tail),
            "scope" | "leave" | "lineseq" => {
                let stmts = stmt_list(n, line);
                if self.walk_block(&stmts, path, tail) {
                    Flow::Terminates
                } else {
                    Flow::Continue
                }
            }
            "leavetry" => {
                // eval BLOCK: a die inside is caught, so the outside
                // always continues.
                let stmts = stmt_list(n, line);
                self.walk_block(&stmts, path, false);
                Flow::Continue
            }
            "leaveloop" => {
                self.walk_loop(line, n, path);
                Flow::Continue
            }
            _ => {
                self.tail_implicit(line, node, path, tail);
                Flow::Continue
            }
        }
    }

    /// `return EXPR` with a single, decomposable expression is split into
    /// its short-circuit routes; anything else is reported verbatim.
    fn emit_return(&mut self, line: Option<u32>, n: &'a OpNode, path: &PathSet) {
        let kids: Vec<&'a OpNode> = n
            .kids
            .iter()
            .map(|k| k.skip_null())
            .filter(|k| k.name != "pushmark")
            .collect();
        if let [expr] = kids[..] {
            self.emit_value(line, "return", expr, path);
        } else {
            let exprs = return_exprs(self.ir, n);
            self.emit(line, "return", exprs, path);
        }
    }

    /// if/elsif/else and the ternary operator
    /// (LOGOP cond_expr: [condition, true arm, false arm]).
    fn walk_cond_expr(
        &mut self,
        line: Option<u32>,
        n: &'a OpNode,
        path: &PathSet,
        tail: bool,
    ) -> Flow {
        let Some(cond) = n.kids.first() else {
            return Flow::Continue;
        };
        let (t, f) = self.split(cond, path, Test::Truth);
        let (t_set, f_set) = (lits_of(t), lits_of(f));
        let t_term = self.walk_arm(n.kids.get(1), line, &t_set, tail);
        let f_term = self.walk_arm(n.kids.get(2), line, &f_set, tail);
        match (t_term, f_term) {
            (true, true) => Flow::Terminates,
            (true, false) => Flow::ContinueWith(f_set),
            (false, true) => Flow::ContinueWith(t_set),
            (false, false) => Flow::Continue,
        }
    }

    /// Statement-level and / or / dor (`STMT if COND`, `unless`, `//`).
    fn walk_logop(&mut self, line: Option<u32>, n: &'a OpNode, path: &PathSet, tail: bool) -> Flow {
        let (Some(cond), Some(arm)) = (n.kids.first(), n.kids.get(1)) else {
            return Flow::Continue;
        };
        let test = if n.name == "dor" {
            Test::Defined
        } else {
            Test::Truth
        };
        let (yes, no) = self.split(cond, path, test);
        // The right arm runs when the condition is true for `and`, and
        // when it is false (or undefined) for `or` / `dor`.
        let (arm_routes, lhs_routes) = if n.name == "and" {
            (yes, no)
        } else {
            (no, yes)
        };
        let arm_term = self.walk_arm(Some(arm), line, &lits_of(arm_routes), tail);
        if tail {
            // On the other side the logop's own value (the left-hand
            // side) is the value of the expression.
            for r in &lhs_routes {
                let v = value_str(r);
                self.paths.push(PathOutcome {
                    line,
                    kind: "implicit".into(),
                    exprs: vec![v],
                    when: r.lits.clone(),
                });
            }
        }
        if arm_term {
            Flow::ContinueWith(lits_of(lhs_routes))
        } else {
            Flow::Continue
        }
    }

    /// A branch arm: a block is walked as a statement list, a single
    /// expression as one statement. Returns whether every route of the arm
    /// terminated (an unreachable arm counts as terminated).
    fn walk_arm(
        &mut self,
        node: Option<&'a OpNode>,
        line: Option<u32>,
        path: &PathSet,
        tail: bool,
    ) -> bool {
        let Some(node) = node else {
            return false;
        };
        let n = node.skip_null();
        match n.name.as_str() {
            "scope" | "leave" | "lineseq" => {
                let stmts = stmt_list(n, line);
                self.walk_block(&stmts, path, tail)
            }
            _ => self.walk_block(&[(line, node)], path, tail),
        }
    }

    /// leaveloop: the loop control (iterator / continuation condition) is
    /// not an atom; only branches in the body are collected. The body may
    /// run zero times, so it never terminates the enclosing block.
    fn walk_loop(&mut self, line: Option<u32>, n: &'a OpNode, path: &PathSet) {
        for k in &n.kids {
            let s = k.skip_null();
            match s.name.as_str() {
                "enteriter" | "enterloop" => {}
                "and" | "or" => {
                    if let Some(body) = s.kids.get(1) {
                        self.walk_arm(Some(body), line, path, false);
                    }
                }
                _ => {
                    self.walk_stmt(line, k, path, false);
                }
            }
        }
    }

    /// An expression statement in tail position is an implicit return.
    fn tail_implicit(&mut self, line: Option<u32>, node: &'a OpNode, path: &PathSet, tail: bool) {
        if tail {
            self.emit_value(line, "implicit", node, path);
        }
    }

    /// Record a value-position expression: decomposed into one outcome per
    /// short-circuit route when possible, otherwise rendered as a whole.
    fn emit_value(&mut self, line: Option<u32>, kind: &str, node: &'a OpNode, path: &PathSet) {
        if !is_decomposable(node) {
            self.emit(line, kind, vec![render(self.ir, node)], path);
            return;
        }
        for conj in path {
            for r in self.decompose(node, conj.clone(), false, false) {
                let v = value_str(&r);
                self.paths.push(PathOutcome {
                    line,
                    kind: kind.to_string(),
                    exprs: vec![v],
                    when: r.lits,
                });
            }
        }
    }

    /// One outcome per conjunction of the path set.
    fn emit(&mut self, line: Option<u32>, kind: &str, exprs: Vec<String>, path: &PathSet) {
        for conj in path {
            self.paths.push(PathOutcome {
                line,
                kind: kind.to_string(),
                exprs: exprs.clone(),
                when: conj.clone(),
            });
        }
    }

    /// Split a guard condition into the routes on which it holds and the
    /// routes on which it does not, each extended with the deciding
    /// literal, for every conjunction of the incoming path set.
    fn split(&mut self, cond: &'a OpNode, path: &PathSet, test: Test) -> (Vec<Route>, Vec<Route>) {
        let mut yes = Vec::new();
        let mut no = Vec::new();
        for conj in path {
            match test {
                Test::Truth => {
                    for r in self.decompose(cond, conj.clone(), false, true) {
                        let atom = self.atom_of(&r);
                        // The expression is true when the deciding atom
                        // has the value `!neg`.
                        if let Some(l) = push_lit(&r.lits, atom, !r.neg) {
                            yes.push(Route {
                                lits: l,
                                atom: Some(atom),
                                ..r.clone()
                            });
                        }
                        if let Some(l) = push_lit(&r.lits, atom, r.neg) {
                            no.push(Route {
                                lits: l,
                                atom: Some(atom),
                                ..r
                            });
                        }
                    }
                }
                Test::Defined => {
                    let text = render(self.ir, cond);
                    let d = self.intern_text(format!("defined({})", text));
                    let route = |lits| Route {
                        lits,
                        atom: None,
                        value: text.clone(),
                        neg: false,
                    };
                    if let Some(l) = push_lit(conj, d, true) {
                        yes.push(route(l));
                    }
                    if let Some(l) = push_lit(conj, d, false) {
                        no.push(route(l));
                    }
                }
            }
        }
        (yes, no)
    }

    /// Enumerate the short-circuit routes of `node` starting from the
    /// conjunction `lits`. `neg` counts enclosing `not`s; `operand` is
    /// true when the node is an operand of `&&` / `||` / `!` or the
    /// condition of `?:` (its leaves become atoms) and false in value
    /// position (`return` / implicit value / `?:` arms).
    fn decompose(
        &mut self,
        node: &'a OpNode,
        lits: Vec<CondLit>,
        neg: bool,
        operand: bool,
    ) -> Vec<Route> {
        let n = strip(node);
        match (n.name.as_str(), &n.kids[..]) {
            ("not", [k]) => self.decompose(k, lits, !neg, operand),
            ("and" | "or", [l, r]) => {
                // The left operand decides the value of `&&` when it is
                // false and of `||` when it is true.
                let lhs_decides = n.name == "or";
                let mut out = Vec::new();
                for lr in self.decompose(l, lits, false, true) {
                    let atom = self.atom_of(&lr);
                    if let Some(sc) = push_lit(&lr.lits, atom, lhs_decides ^ lr.neg) {
                        out.push(Route {
                            lits: sc,
                            atom: Some(atom),
                            value: lr.value.clone(),
                            neg: lr.neg ^ neg,
                        });
                    }
                    if let Some(go) = push_lit(&lr.lits, atom, !lhs_decides ^ lr.neg) {
                        out.extend(self.decompose(r, go, neg, true));
                    }
                }
                out
            }
            ("dor", [l, r]) => {
                let text = render(self.ir, l);
                let d = self.intern_text(format!("defined({})", text));
                let mut out = Vec::new();
                if let Some(sc) = push_lit(&lits, d, true) {
                    out.push(Route {
                        lits: sc,
                        atom: None,
                        value: text,
                        neg,
                    });
                }
                if let Some(go) = push_lit(&lits, d, false) {
                    out.extend(self.decompose(r, go, neg, operand));
                }
                out
            }
            ("cond_expr", [c, t, f]) => {
                let mut out = Vec::new();
                for cr in self.decompose(c, lits, false, true) {
                    let atom = self.atom_of(&cr);
                    if let Some(go) = push_lit(&cr.lits, atom, !cr.neg) {
                        out.extend(self.decompose(t, go, neg, operand));
                    }
                    if let Some(go) = push_lit(&cr.lits, atom, cr.neg) {
                        out.extend(self.decompose(f, go, neg, operand));
                    }
                }
                out
            }
            _ => {
                let text = render(self.ir, n);
                let atom = operand.then(|| self.intern_text(text.clone()));
                vec![Route {
                    lits,
                    atom,
                    value: text,
                    neg,
                }]
            }
        }
    }

    /// The atom deciding a route, interning the route's value on demand
    /// (an opaque `//` left-hand side whose truth is tested later).
    fn atom_of(&mut self, r: &Route) -> usize {
        match r.atom {
            Some(a) => a,
            None => self.intern_text(r.value.clone()),
        }
    }

    fn intern_text(&mut self, text: String) -> usize {
        if let Some(i) = self.conds.iter().position(|c| *c == text) {
            i
        } else {
            self.conds.push(text);
            self.conds.len() - 1
        }
    }

    /// Enumerate every assignment and look up the first path whose `when`
    /// holds under it.
    fn build_table(&self) -> Option<Vec<TableRow>> {
        let n = self.conds.len();
        if n == 0 || n > MAX_TABLE_CONDS {
            return None;
        }
        let mut rows = Vec::with_capacity(1 << n);
        for mask in 0..(1u32 << n) {
            let inputs: Vec<bool> = (0..n).map(|i| mask >> (n - 1 - i) & 1 == 1).collect();
            let path = self
                .paths
                .iter()
                .position(|p| p.when.iter().all(|l| inputs[l.cond] == l.value));
            rows.push(TableRow { inputs, path });
        }
        Some(rows)
    }
}

/// Split the kids of a block op at COPs into (line, statement) pairs.
/// enter / entertry / unstack do not count as statements.
fn stmt_list<'a>(n: &'a OpNode, inherited: Option<u32>) -> Vec<(Option<u32>, &'a OpNode)> {
    let mut line = inherited;
    let mut out = Vec::new();
    for k in &n.kids {
        if k.class == OpClass::Cop {
            if let crate::ir::OpDetail::Cop { line: l, .. } = &k.detail {
                line = Some(*l);
            }
            continue;
        }
        match k.name.as_str() {
            "enter" | "entertry" | "unstack" => {}
            _ => out.push((line, k)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logic_of(golden: &str) -> LogicSpec {
        let ir: SubIr = serde_json::from_str(golden).expect("golden fixture deserializes");
        analyze_logic(&ir)
    }

    fn whens(p: &PathOutcome) -> Vec<(usize, bool)> {
        p.when.iter().map(|l| (l.cond, l.value)).collect()
    }

    fn table_path(l: &LogicSpec, inputs: &[bool]) -> Option<usize> {
        l.table
            .as_ref()
            .unwrap()
            .iter()
            .find(|r| r.inputs == inputs)
            .and_then(|r| r.path)
    }

    #[test]
    fn bool_return_is_decomposed() {
        // sub { my ($a, $b, $c) = @_; return ($a && $b) || !$c }
        let l = logic_of(include_str!("../../../../t/golden/bool_return.json"));
        assert_eq!(l.conds, ["$a", "$b", "$c"]);
        let p = &l.paths;
        assert_eq!(p.len(), 3);
        assert!(p.iter().all(|p| p.kind == "return"));
        assert_eq!(p[0].exprs, ["!$c"]);
        assert_eq!(whens(&p[0]), [(0, false)]);
        assert_eq!(p[1].exprs, ["$b"]);
        assert_eq!(whens(&p[1]), [(0, true), (1, true)]);
        assert_eq!(p[2].exprs, ["!$c"]);
        assert_eq!(whens(&p[2]), [(0, true), (1, false)]);
        assert_eq!(l.table.as_ref().unwrap().len(), 8);
        assert_eq!(table_path(&l, &[false, true, true]), Some(0));
        assert_eq!(table_path(&l, &[true, false, false]), Some(2));
        assert_eq!(table_path(&l, &[true, true, false]), Some(1));
    }

    #[test]
    fn compound_guard_shares_atoms() {
        // sub { my ($x, $y) = @_; if ($x && !$y) { "a" } elsif ($x) { "b" } else { "c" } }
        let l = logic_of(include_str!("../../../../t/golden/compound_guard.json"));
        assert_eq!(l.conds, ["$x", "$y"]);
        let p = &l.paths;
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].exprs, ["\"a\""]);
        assert_eq!(whens(&p[0]), [(0, true), (1, false)]);
        assert_eq!(p[1].exprs, ["\"b\""]);
        assert_eq!(whens(&p[1]), [(0, true), (1, true)]);
        assert_eq!(p[2].exprs, ["\"c\""]);
        assert_eq!(whens(&p[2]), [(0, false)]);
        let table = l.table.as_ref().unwrap();
        assert_eq!(table.len(), 4);
        assert!(
            table.iter().all(|r| r.path.is_some()),
            "no unreachable rows"
        );
    }

    #[test]
    fn push_lit_prunes_contradictions() {
        let base = vec![CondLit {
            cond: 0,
            value: true,
        }];
        assert_eq!(push_lit(&base, 0, true), Some(base.clone()));
        assert_eq!(push_lit(&base, 0, false), None);
        assert_eq!(push_lit(&base, 1, false).map(|v| v.len()), Some(2));
    }
}
