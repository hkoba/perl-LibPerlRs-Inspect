//! M3: structural path analysis of guard conditions (truth table) v0.
//!
//! Walks the sub body statement by statement, accumulating a path condition
//! (a conjunction of atomic conditions' truth values) for each branch of
//! and / or / dor / cond_expr, and records each outcome (return / die /
//! croak / implicit final expression) together with "which combination of
//! conditions reaches it". If there are few atoms (at most MAX_TABLE_CONDS),
//! a truth table over all combinations is generated as well.
//!
//! v0 simplifications (future work):
//! - Atomic conditions are identified by their rendered text (semantic
//!   implication, e.g. `$v > 10` ⇒ `$v > 5`, is not considered. The truth
//!   table may contain semantically unreachable rows).
//! - After a branch merges, its literal is dropped if both arms survive
//!   (early-return form: only when one arm always ends in return/die is the
//!   negated literal carried over to the continuing side).
//! - Ternaries etc. buried inside expressions (e.g. the RHS of an
//!   assignment) are not picked up as atoms.
//! - A die inside an eval BLOCK is caught, but is still reported with kind "die".
//! - Loops do not include the loop control condition as an atom; only the
//!   branches in the body are picked up.

use serde::{Deserialize, Serialize};

use crate::ir::{OpClass, OpDetail, OpNode, SubIr};
use crate::render::render;

use super::retspec::{croak_call, return_exprs};

/// Above this many atoms, no truth table (2^n rows) is generated
pub const MAX_TABLE_CONDS: usize = 6;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogicSpec {
    /// Rendered text of the atomic conditions (in order of first appearance). Indexed by `when` / `table`
    pub conds: Vec<String>,
    /// Outcomes and their path conditions (in source order)
    pub paths: Vec<PathOutcome>,
    /// Truth table. None if conds is empty or exceeds MAX_TABLE_CONDS
    pub table: Option<Vec<TableRow>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathOutcome {
    pub line: Option<u32>,
    /// return | die | croak | confess | implicit
    pub kind: String,
    pub exprs: Vec<String>,
    /// Condition under which this path is reached (a conjunction of literals)
    pub when: Vec<CondLit>,
}

/// A literal in a path condition: conds[cond] has the value `value`
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CondLit {
    pub cond: usize,
    pub value: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableRow {
    /// Truth assignment in the same order as conds (enumerated with conds[0] as the most significant bit)
    pub inputs: Vec<bool>,
    /// Index of the first entry in paths whose when holds (None if none does)
    pub path: Option<usize>,
}

pub fn analyze_logic(ir: &SubIr) -> LogicSpec {
    let mut w = Walker {
        ir,
        conds: Vec::new(),
        paths: Vec::new(),
    };
    let stmts = super::statements(ir);
    w.walk_block(&stmts, &[], true);
    let table = w.build_table();
    LogicSpec {
        conds: w.conds,
        paths: w.paths,
        table,
    }
}

/// Result of processing a statement: how the following statements continue
enum Flow {
    /// Continues normally
    Continue,
    /// One arm of this statement always terminated, so the continuing side proceeds with extra literals
    ContinueWith(Vec<CondLit>),
    /// All paths terminated with return/die (subsequent statements are unreachable)
    Terminates,
}

struct Walker<'a> {
    ir: &'a SubIr,
    conds: Vec<String>,
    paths: Vec<PathOutcome>,
}

impl<'a> Walker<'a> {
    /// Process a statement list in order. Returns whether all paths terminated.
    /// `tail` is whether this list is in tail position of the sub (or arm).
    fn walk_block(&mut self, stmts: &[(Option<u32>, &'a OpNode)], path: &[CondLit], tail: bool) -> bool {
        let mut path = path.to_vec();
        for (i, (line, node)) in stmts.iter().enumerate() {
            let stmt_tail = tail && i + 1 == stmts.len();
            match self.walk_stmt(*line, node, &path, stmt_tail) {
                Flow::Terminates => return true,
                Flow::ContinueWith(extra) => path.extend(extra),
                Flow::Continue => {}
            }
        }
        false
    }

    fn walk_stmt(&mut self, line: Option<u32>, node: &'a OpNode, path: &[CondLit], tail: bool) -> Flow {
        let n = node.skip_null();
        match n.name.as_str() {
            "return" => {
                let exprs = return_exprs(self.ir, n);
                self.emit(line, "return", exprs, path);
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
                // eval BLOCK: a die inside is caught, so the outside always continues
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

    /// if/elsif/else and the ternary operator (LOGOP cond_expr: [cond, true arm, false arm])
    fn walk_cond_expr(&mut self, line: Option<u32>, n: &'a OpNode, path: &[CondLit], tail: bool) -> Flow {
        let Some(cond) = n.kids.first() else {
            return Flow::Continue;
        };
        let (idx, pol) = self.intern_cond(cond);
        let mut t_path = path.to_vec();
        t_path.push(CondLit { cond: idx, value: pol });
        let t_term = self.walk_arm(n.kids.get(1), line, t_path, tail);
        let mut f_path = path.to_vec();
        f_path.push(CondLit { cond: idx, value: !pol });
        let f_term = self.walk_arm(n.kids.get(2), line, f_path, tail);
        match (t_term, f_term) {
            (true, true) => Flow::Terminates,
            (true, false) => Flow::ContinueWith(vec![CondLit { cond: idx, value: !pol }]),
            (false, true) => Flow::ContinueWith(vec![CondLit { cond: idx, value: pol }]),
            (false, false) => Flow::Continue,
        }
    }

    /// Statement-level and / or / dor (`STMT if COND` / `unless` / `//`)
    fn walk_logop(&mut self, line: Option<u32>, n: &'a OpNode, path: &[CondLit], tail: bool) -> Flow {
        let (Some(cond), Some(arm)) = (n.kids.first(), n.kids.get(1)) else {
            return Flow::Continue;
        };
        // arm_when: the atom's value when the right arm runs
        let (idx, arm_when) = match n.name.as_str() {
            "and" => self.intern_cond(cond),
            "or" => {
                let (i, pol) = self.intern_cond(cond);
                (i, !pol)
            }
            // dor: the right arm runs only when the LHS is undef
            _ => (
                self.intern_text(format!("defined({})", render(self.ir, cond))),
                false,
            ),
        };
        let mut a_path = path.to_vec();
        a_path.push(CondLit { cond: idx, value: arm_when });
        let arm_term = self.walk_arm(Some(arm), line, a_path, tail);
        if tail {
            // on the other side, the logop's own value (= the LHS value) is the expression's value
            let mut f_path = path.to_vec();
            f_path.push(CondLit { cond: idx, value: !arm_when });
            self.emit(line, "implicit", vec![render(self.ir, cond)], &f_path);
        }
        if arm_term {
            Flow::ContinueWith(vec![CondLit { cond: idx, value: !arm_when }])
        } else {
            Flow::Continue
        }
    }

    /// A branch arm: walked as a statement list if it is a block, or as a single
    /// statement if it is a lone expression. Returns whether all paths of the arm terminated.
    fn walk_arm(&mut self, node: Option<&'a OpNode>, line: Option<u32>, path: Vec<CondLit>, tail: bool) -> bool {
        let Some(node) = node else {
            return false;
        };
        let n = node.skip_null();
        match n.name.as_str() {
            "scope" | "leave" | "lineseq" => {
                let stmts = stmt_list(n, line);
                self.walk_block(&stmts, &path, tail)
            }
            _ => self.walk_block(&[(line, node)], &path, tail),
        }
    }

    /// leaveloop: loop control (iter / continuation condition) is not included as
    /// an atom; only the branches in the body are picked up. The body may run
    /// zero times, so it is never treated as terminating.
    fn walk_loop(&mut self, line: Option<u32>, n: &'a OpNode, path: &[CondLit]) {
        for k in &n.kids {
            let s = k.skip_null();
            match s.name.as_str() {
                "enteriter" | "enterloop" => {}
                "and" | "or" => {
                    if let Some(body) = s.kids.get(1) {
                        self.walk_arm(Some(body), line, path.to_vec(), false);
                    }
                }
                _ => {
                    self.walk_stmt(line, k, path, false);
                }
            }
        }
    }

    /// Record an expression statement in tail position as an implicit return value
    fn tail_implicit(&mut self, line: Option<u32>, node: &'a OpNode, path: &[CondLit], tail: bool) {
        if tail {
            self.emit(line, "implicit", vec![render(self.ir, node)], path);
        }
    }

    fn emit(&mut self, line: Option<u32>, kind: &str, exprs: Vec<String>, when: &[CondLit]) {
        self.paths.push(PathOutcome {
            line,
            kind: kind.to_string(),
            exprs,
            when: when.to_vec(),
        });
    }

    /// Register a condition node as an atom (identified by rendered text) and
    /// return (index, the atom's value corresponding to the true arm). `not` flips the atom.
    /// An elsif condition is wrapped in lineseq(ex-nextstate, cond), so unwrap it.
    fn intern_cond(&mut self, node: &'a OpNode) -> (usize, bool) {
        let mut n = node.skip_null();
        if n.name == "lineseq" {
            if let Some(last) = n.kids.iter().filter(|k| k.class != OpClass::Cop).last() {
                n = last.skip_null();
            }
        }
        if n.name == "not" {
            if let Some(inner) = n.kids.first() {
                return (self.intern_text(render(self.ir, inner)), false);
            }
        }
        (self.intern_text(render(self.ir, n)), true)
    }

    fn intern_text(&mut self, text: String) -> usize {
        if let Some(i) = self.conds.iter().position(|c| *c == text) {
            i
        } else {
            self.conds.push(text);
            self.conds.len() - 1
        }
    }

    /// Enumerate all combinations and, for each row, find the first path whose when holds
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

/// Split the children of a block op at COPs into a sequence of (line number, statement).
/// enter / entertry / unstack are not counted as statements.
fn stmt_list<'a>(n: &'a OpNode, inherited: Option<u32>) -> Vec<(Option<u32>, &'a OpNode)> {
    let mut line = inherited;
    let mut out = Vec::new();
    for k in &n.kids {
        if k.class == OpClass::Cop {
            if let OpDetail::Cop { line: l, .. } = &k.detail {
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
