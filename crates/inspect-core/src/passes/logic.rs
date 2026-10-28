//! M3: structural path analysis of guard conditions (truth tables).
//!
//! The sub body is walked statement by statement. Every branch point
//! (`if` / `elsif` / `else`, `STMT if COND`, `unless`, the ternary
//! operator, `//`) and every boolean operator reachable from a decomposed
//! position contributes *atomic conditions*. Each reachable outcome
//! (`return`, `die`, `croak` / `confess`, `exit`, or the implicit final
//! expression) is recorded together with the conjunction of literals that
//! leads to it. When the number of atoms is small (at most
//! `MAX_TABLE_CONDS`) a full truth table over all combinations is produced
//! as well.
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
//! Atoms are identified not by their text but by a canonical key (see
//! [`super::logic_canon`]): the variables an expression reads, each with
//! its *generation* (how many times it has been modified so far in walk
//! order), and comparisons brought to `<` / `==` form. So
//! `defined(my $x = f())` and a later `defined($x)` are one atom (the
//! value of an assignment is its target), `length($s) >= 8` and
//! `length($s) < 8` are one atom with opposite polarity, while `$x` before
//! and after `$x = g()` are two. Modifications in one arm of a branch
//! separate every later atom from those before the branch; a loop
//! separates the atoms before it, inside it and after it for every
//! variable modified in the loop. Plain sub-call arguments (`f($x)`,
//! which perl marks as modifiable because `@_` aliases them) are not
//! modifications unless `WalkOpts::alias_modifies` is set.
//!
//! Every atom records where it occurs (`cond_sites`): the node id of the
//! op whose text was interned, the line of the enclosing statement, the
//! text at that site and whether the site is the negation of the column.
//! The column's text is that of its first site. Perl keeps line numbers
//! per statement (COP), so the atoms of a condition spread over several
//! lines all report the statement's line.
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
//! Each path also lists the statements executed on its route before the
//! outcome (`stmts`), restricted to statements whose execution depends on
//! at least one atom: statements before the first branch (common to every
//! route) are omitted. The literals of a branch whose arms fall through
//! are carried past the branch, together with the statements of each arm,
//! so a later outcome is reported once per arm route; when that
//! continuation would fan out into more than `MAX_PATH_SET` conjunctions
//! the branch literals (and the arm statements) are dropped instead.
//! Loops are not recorded as statements (their inner branches still are);
//! a loop body may run zero times, so nothing from inside a loop is
//! carried past it.
//!
//! # Ranges
//!
//! [`analyze_logic_range`] analyses the statements of a source line range
//! as one block: the block's own exits (`return`, `die`, …) are outcomes
//! as usual, and every route that survives its last statement is reported
//! as a `fallthrough` outcome. Inside a range `next` / `last` / `redo`
//! terminate a route as well (kinds `next` / `last` / `redo`). The range
//! is snapped to whole statements of one statement list: when it touches
//! a single compound statement without its header line and lies inside
//! one of that statement's arms or bodies, the arm is the block (see
//! [`RangeLogic::in_block_of`]); otherwise the block is the contiguous
//! run of statements the range touches.
//!
//! Remaining limitations:
//! - atoms are compared textually; semantic implication such as
//!   `$v > 10` => `$v > 5` is not considered, so a truth table may contain
//!   rows that are semantically unreachable;
//! - `die` inside `eval BLOCK` is caught, but its kind is still reported
//!   as `die`, and literals from inside the eval are not carried past it;
//! - loop control conditions are not atoms; only branches in the loop body
//!   are collected;
//! - `render` does not track precedence, so an opaque `!($a && $b)` (for
//!   example as a `die` argument) is displayed as `!$a && $b`;
//! - `a >= b` is taken as the negation of `a < b` and `a != b` as that of
//!   `a == b`, which assumes a total order: false for NaN operands and for
//!   overloaded operators that are not mutually consistent; `==` / `eq`
//!   operands are reordered, which assumes a symmetric overload;
//! - a variable modified through an alias (`f($x)`, `for (@a)`, a reference
//!   taken earlier) is not seen unless `alias_modifies` is set.
//!
//! [`Identity`] relaxes atom identity on request: `Expr` ignores
//! modifications and keys sites by their expression (an assignment is the
//! expression it is written as), `Var` ignores modifications and takes an
//! assignment for its target, so a table can show one column per
//! variable test even when the variable is reassigned between the tests.
//! Both can merge tests that see different values, which is the point:
//! the table then describes the program's shape, not its semantics.

use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::ir::{OpClass, OpNode, SubIr};
use crate::render::render;

use super::logic_canon::{self, Canon, Env};
use super::retspec::{croak_call, return_exprs};
use super::{Stmt, block_statements, has_implicit_value, line_span, nested_blocks, statements};

/// Above this many atoms no truth table (2^n rows) is generated.
pub const MAX_TABLE_CONDS: usize = 6;

/// Upper bound on the number of conjunctions carried past a branch;
/// beyond it the branch literals are dropped.
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
    /// Where each atom of `conds` occurs (index-parallel to `conds`):
    /// one entry per op whose text was interned, several when the same
    /// text appears more than once.
    #[serde(default)]
    pub cond_sites: Vec<Vec<CondSite>>,
}

/// An occurrence of an atom: the op node and the line of its statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CondSite {
    pub node: u32,
    pub line: Option<u32>,
    /// The site's truth is the negation of the column (`length($s) < 8`
    /// at a site of the column `length($s) >= 8`).
    #[serde(default)]
    pub neg: bool,
    /// Rendered text at this site.
    #[serde(default)]
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathOutcome {
    pub line: Option<u32>,
    /// return | die | croak | confess | exit | implicit | fallthrough |
    /// next | last | redo
    pub kind: String,
    pub exprs: Vec<String>,
    /// Conjunction of literals under which this outcome is reached.
    pub when: Vec<CondLit>,
    /// Statements executed on this route before the outcome whose
    /// execution depends on at least one atom (source order).
    #[serde(default)]
    pub stmts: Vec<StmtRef>,
}

/// A rendered statement with its source line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StmtRef {
    pub line: Option<u32>,
    pub text: String,
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

/// When two sites are the same atom (one column of the truth table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Identity {
    /// By value: the variables an expression reads, at their generation
    /// (sites separated by a modification of a variable are distinct),
    /// and an assignment stands for its target afterwards. With
    /// `defined($x = a()) && $x >= 3 || defined($x = b()) && $x < 3`:
    /// four atoms.
    #[default]
    Strict,
    /// By expression: modifications are ignored and an assignment is the
    /// expression it is written as. Three atoms in the example:
    /// `defined($x = a())`, `defined($x = b())`, `$x >= 3`.
    Expr,
    /// By variable: modifications are ignored and an assignment is its
    /// target, so every test of `$x` is about "`$x`". Two atoms in the
    /// example, `defined($x)` and `$x >= 3`; atoms are displayed with
    /// assignments collapsed to their target.
    Var,
}

impl Identity {
    pub const ALL: [Identity; 3] = [Identity::Strict, Identity::Expr, Identity::Var];

    pub fn as_str(self) -> &'static str {
        match self {
            Identity::Strict => "strict",
            Identity::Expr => "expr",
            Identity::Var => "var",
        }
    }

    pub fn parse(s: &str) -> Option<Identity> {
        Identity::ALL.into_iter().find(|i| i.as_str() == s)
    }

    /// The mode after this one, cyclically (strict → expr → var → strict).
    pub fn next(self) -> Identity {
        let i = Identity::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Identity::ALL[(i + 1) % Identity::ALL.len()]
    }

    /// Whether variable modifications separate atoms.
    pub fn tracks_generations(self) -> bool {
        self == Identity::Strict
    }
}

/// How a statement list is walked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WalkOpts {
    /// The list is the tail of a sub: its last expression is the implicit
    /// return value. Otherwise routes surviving the last statement become
    /// `fallthrough` outcomes.
    pub tail: bool,
    /// `next` / `last` / `redo` terminate a route (as outcomes of that
    /// kind) instead of being plain statements.
    pub loop_control_outcomes: bool,
    /// Treat sub-call argument aliasing (`f($x)`, `$x->m`, `map {} @a`:
    /// `OPf_MOD` without `OPf_REF`) as a modification of the variable.
    pub alias_modifies: bool,
    /// When two sites are one atom.
    pub identity: Identity,
}

/// Options of [`analyze_logic_with`] / [`analyze_logic_range_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LogicOpts {
    /// See [`WalkOpts::alias_modifies`].
    #[serde(default)]
    pub alias_modifies: bool,
    /// See [`Identity`].
    #[serde(default)]
    pub identity: Identity,
}

/// Result of [`analyze_logic_range`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RangeLogic {
    pub logic: LogicSpec,
    /// Node ids of the statements analysed, in order.
    pub stmt_ids: Vec<u32>,
    /// Source lines actually covered by those statements.
    pub lines: (u32, u32),
    /// `lines` differs from the requested range.
    pub snapped: bool,
    /// When the block is an arm or body of a compound statement, that
    /// statement's node id.
    pub in_block_of: Option<u32>,
    /// The block ends the sub: its last expression was treated as the
    /// implicit return value instead of a fall-through.
    pub tail: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RangeError {
    /// The body has no statements at all.
    Empty,
    /// No statement touches the requested lines.
    NoStatements { lines: (u32, u32) },
}

impl std::fmt::Display for RangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RangeError::Empty => write!(f, "the body has no statements"),
            RangeError::NoStatements { lines: (lo, hi) } => {
                write!(f, "no statement on lines {lo}-{hi}")
            }
        }
    }
}

impl std::error::Error for RangeError {}

/// Logic of the whole sub body.
pub fn analyze_logic(ir: &SubIr) -> LogicSpec {
    analyze_logic_with(ir, &LogicOpts::default())
}

/// [`analyze_logic`] with options.
pub fn analyze_logic_with(ir: &SubIr, lopts: &LogicOpts) -> LogicSpec {
    let opts = WalkOpts {
        tail: has_implicit_value(ir),
        loop_control_outcomes: false,
        alias_modifies: lopts.alias_modifies,
        identity: lopts.identity,
    };
    analyze_logic_stmts(ir, &statements(ir), &opts)
}

/// Logic of an arbitrary statement list of `ir` (see [`WalkOpts`]).
pub fn analyze_logic_stmts(ir: &SubIr, stmts: &[Stmt<'_>], opts: &WalkOpts) -> LogicSpec {
    let mut w = Walker {
        ir,
        opts: *opts,
        conds: Vec::new(),
        keys: Vec::new(),
        col_neg: Vec::new(),
        cond_sites: Vec::new(),
        cur_line: None,
        env: Env::new(opts.identity.tracks_generations()),
        paths: Vec::new(),
    };
    w.walk_top(stmts);
    let table = w.build_table();
    LogicSpec {
        conds: w.conds,
        paths: w.paths,
        table,
        cond_sites: w.cond_sites,
    }
}

fn intersects(span: Option<(u32, u32)>, lo: u32, hi: u32) -> bool {
    span.is_some_and(|(a, b)| a <= hi && b >= lo)
}

fn union(a: Option<(u32, u32)>, b: Option<(u32, u32)>) -> Option<(u32, u32)> {
    match (a, b) {
        (Some((a0, a1)), Some((b0, b1))) => Some((a0.min(b0), a1.max(b1))),
        (x, None) | (None, x) => x,
    }
}

/// Logic of the statements on a range of source lines, treated as one
/// block (see the module documentation, section *Ranges*).
pub fn analyze_logic_range(
    ir: &SubIr,
    lines: RangeInclusive<u32>,
) -> Result<RangeLogic, RangeError> {
    analyze_logic_range_with(ir, lines, &LogicOpts::default())
}

/// [`analyze_logic_range`] with options.
pub fn analyze_logic_range_with(
    ir: &SubIr,
    lines: RangeInclusive<u32>,
    lopts: &LogicOpts,
) -> Result<RangeLogic, RangeError> {
    let (lo, hi) = (*lines.start(), *lines.end());
    let mut list: Vec<Stmt<'_>> = statements(ir);
    if list.is_empty() {
        return Err(RangeError::Empty);
    }
    let mut tail_ctx = has_implicit_value(ir);
    let mut in_block_of = None;
    loop {
        let spans: Vec<Option<(u32, u32)>> = list.iter().map(|(l, n)| line_span(n, *l)).collect();
        let hits: Vec<usize> = (0..list.len())
            .filter(|&i| intersects(spans[i], lo, hi))
            .collect();
        let (Some(&first), Some(&last)) = (hits.first(), hits.last()) else {
            return Err(RangeError::NoStatements { lines: (lo, hi) });
        };
        if first == last {
            let (line, node) = list[first];
            let header_selected = line.is_some_and(|l| lo <= l && l <= hi);
            if !header_selected {
                let blocks = nested_blocks(node, line);
                let mut inter = blocks.iter().filter(|b| {
                    b.stmts
                        .iter()
                        .any(|(l, n)| intersects(line_span(n, *l), lo, hi))
                });
                if let (Some(b), None) = (inter.next(), inter.next()) {
                    tail_ctx = tail_ctx && first + 1 == list.len() && b.role.carries_tail();
                    in_block_of = Some(node.id);
                    list = b.stmts.clone();
                    continue;
                }
            }
        }
        let sel = &list[first..=last];
        let tail = tail_ctx && last + 1 == list.len();
        let span = spans[first..=last]
            .iter()
            .fold(None, |acc, s| union(acc, *s))
            .unwrap_or((lo, hi));
        let opts = WalkOpts {
            tail,
            loop_control_outcomes: true,
            alias_modifies: lopts.alias_modifies,
        identity: lopts.identity,
        };
        let logic = analyze_logic_stmts(ir, sel, &opts);
        return Ok(RangeLogic {
            logic,
            stmt_ids: sel.iter().map(|(_, n)| n.id).collect(),
            lines: span,
            snapped: span != (lo, hi),
            in_block_of,
            tail,
        });
    }
}

/// One conjunction of a path set together with the statements recorded on
/// its route.
#[derive(Debug, Clone, PartialEq)]
struct Conj {
    lits: Vec<CondLit>,
    stmts: Vec<StmtRef>,
}

/// A set of pairwise exclusive conjunctions. A single empty conjunction
/// means "unconditionally reachable"; an empty set means "unreachable".
type PathSet = Vec<Conj>;

fn unconditional() -> PathSet {
    vec![Conj {
        lits: Vec::new(),
        stmts: Vec::new(),
    }]
}

/// One short-circuit route through a boolean expression.
#[derive(Debug, Clone)]
struct Route {
    /// Complete path condition (incoming literals plus those of the route).
    lits: Vec<CondLit>,
    /// Statements recorded on the incoming conjunction.
    stmts: Vec<StmtRef>,
    /// Atom whose truth decides the expression on this route, if one has
    /// been interned (`None` for an opaque `//` left-hand side).
    atom: Option<Atom>,
    /// Canonical identity of an opaque `//` left-hand side, for interning
    /// it on demand without rescanning its modifications.
    canon: Option<Canon>,
    /// Rendered value of the expression on this route (before negation).
    value: String,
    /// Node id of the op `value` renders (the atom's site when it is
    /// interned later).
    node: u32,
    /// The value is negated an odd number of times by enclosing `not`s.
    neg: bool,
}

/// An interned atom as seen from one site: `neg` says the site's truth is
/// the column's negation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Atom {
    idx: usize,
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
    /// Some routes terminated or changed; the rest continue with this
    /// path set.
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

fn conjs_of(routes: Vec<Route>) -> PathSet {
    routes
        .into_iter()
        .map(|r| Conj {
            lits: r.lits,
            stmts: r.stmts,
        })
        .collect()
}

/// The unconditional path set.
fn is_unconditional(path: &PathSet) -> bool {
    path.len() == 1 && path[0].lits.is_empty()
}

struct Walker<'a> {
    ir: &'a SubIr,
    opts: WalkOpts,
    conds: Vec<String>,
    /// Canonical key of each atom (index-parallel to `conds`).
    keys: Vec<String>,
    /// The column's polarity relative to its key: that of its first site.
    col_neg: Vec<bool>,
    cond_sites: Vec<Vec<CondSite>>,
    /// Line of the statement being walked (recorded with each atom).
    cur_line: Option<u32>,
    /// Generations of the variables modified so far.
    env: Env,
    paths: Vec<PathOutcome>,
}

impl<'a> Walker<'a> {
    /// Walk the top-level statement list; routes surviving it become
    /// `fallthrough` outcomes unless the list is a sub's tail.
    fn walk_top(&mut self, stmts: &[Stmt<'a>]) {
        let remaining = self.walk_block(stmts, &unconditional(), self.opts.tail);
        if self.opts.tail {
            return;
        }
        if let Some(set) = remaining {
            // Reported at the last line of the block's last statement.
            let line = stmts
                .last()
                .and_then(|(l, n)| line_span(n, *l).map(|(_, hi)| hi).or(*l));
            self.emit(line, "fallthrough", Vec::new(), &set);
        }
    }

    /// Walk a statement list. Returns the path set surviving the last
    /// statement, or `None` when every route terminated. `tail` says
    /// whether the list sits in tail position of the sub (or of an arm),
    /// so that its last expression is an implicit return.
    fn walk_block(&mut self, stmts: &[Stmt<'a>], path: &PathSet, tail: bool) -> Option<PathSet> {
        if path.is_empty() {
            return None;
        }
        let mut path = path.clone();
        for (i, (line, node)) in stmts.iter().enumerate() {
            let stmt_tail = tail && i + 1 == stmts.len();
            match self.walk_stmt(*line, node, &path, stmt_tail) {
                Flow::Terminates => return None,
                Flow::ContinueWith(set) => {
                    if set.is_empty() {
                        return None;
                    }
                    if set.len() <= MAX_PATH_SET {
                        path = set;
                    }
                }
                Flow::Continue => {}
            }
        }
        Some(path)
    }

    fn walk_stmt(
        &mut self,
        line: Option<u32>,
        node: &'a OpNode,
        path: &PathSet,
        tail: bool,
    ) -> Flow {
        self.cur_line = line;
        let n = node.skip_null();
        match n.name.as_str() {
            "return" => {
                self.emit_return(line, n, path);
                Flow::Terminates
            }
            "die" => {
                self.note_mods(n);
                let exprs = return_exprs(self.ir, n);
                self.emit(line, "die", exprs, path);
                Flow::Terminates
            }
            "exit" => {
                self.note_mods(n);
                let exprs = return_exprs(self.ir, n);
                self.emit(line, "exit", exprs, path);
                Flow::Terminates
            }
            "next" | "last" | "redo" if self.opts.loop_control_outcomes => {
                self.note_mods(n);
                let exprs = return_exprs(self.ir, n);
                self.emit(line, &n.name, exprs, path);
                Flow::Terminates
            }
            "entersub" => {
                if let Some((via, msg)) = croak_call(self.ir, n) {
                    self.note_mods(n);
                    self.emit(line, &via, msg.into_iter().collect(), path);
                    Flow::Terminates
                } else {
                    self.plain_stmt(line, node, path, tail)
                }
            }
            "cond_expr" => self.walk_cond_expr(line, n, path, tail),
            "and" | "or" | "dor" => self.walk_logop(line, n, path, tail),
            "scope" | "leave" | "lineseq" => {
                let stmts = block_statements(n, line);
                match self.walk_block(&stmts, path, tail) {
                    None => Flow::Terminates,
                    Some(set) => Flow::ContinueWith(set),
                }
            }
            "leavetry" => {
                // eval BLOCK: a die inside is caught, so the outside
                // always continues.
                let stmts = block_statements(n, line);
                self.walk_block(&stmts, path, false);
                Flow::Continue
            }
            "leaveloop" => {
                self.walk_loop(line, n, path);
                Flow::Continue
            }
            _ => self.plain_stmt(line, node, path, tail),
        }
    }

    /// Apply the modifications made by the ops under `n` to the
    /// generations (for statements that are not decomposed).
    fn note_mods(&mut self, n: &OpNode) {
        let mut mods = Vec::new();
        logic_canon::modified_vars(n, &self.opts, &mut mods);
        self.env.bump_all(mods);
    }

    /// A plain statement: in tail position it is the implicit return
    /// value; otherwise it is recorded on every route of the path set when
    /// the set is conditional.
    fn plain_stmt(
        &mut self,
        line: Option<u32>,
        node: &'a OpNode,
        path: &PathSet,
        tail: bool,
    ) -> Flow {
        if tail {
            self.emit_value(line, "implicit", node, path);
            Flow::Continue
        } else if !is_unconditional(path) {
            self.note_mods(node);
            let text = render(self.ir, node);
            let set = path
                .iter()
                .map(|c| {
                    let mut c = c.clone();
                    c.stmts.push(StmtRef {
                        line,
                        text: text.clone(),
                    });
                    c
                })
                .collect();
            Flow::ContinueWith(set)
        } else {
            self.note_mods(node);
            Flow::Continue
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
            self.note_mods(n);
            let exprs = return_exprs(self.ir, n);
            self.emit(line, "return", exprs, path);
        }
    }

    /// Combine the path sets surviving the arms of a branch.
    fn merge(rems: Vec<Option<PathSet>>) -> Flow {
        let mut out: PathSet = Vec::new();
        let mut any = false;
        for r in rems.into_iter().flatten() {
            any = true;
            out.extend(r);
        }
        if any {
            Flow::ContinueWith(out)
        } else {
            Flow::Terminates
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
        let (t_set, f_set) = (conjs_of(t), conjs_of(f));
        let before = self.env.clone();
        let t_rem = self.walk_arm(n.kids.get(1), line, &t_set, tail);
        let after_t = std::mem::replace(&mut self.env, before);
        let f_rem = self.walk_arm(n.kids.get(2), line, &f_set, tail);
        self.env.join(&after_t);
        Self::merge(vec![t_rem, f_rem])
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
        let before = self.env.clone();
        let arm_rem = self.walk_arm(Some(arm), line, &conjs_of(arm_routes), tail);
        self.env.join(&before);
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
                    stmts: r.stmts.clone(),
                });
            }
        }
        Self::merge(vec![arm_rem, Some(conjs_of(lhs_routes))])
    }

    /// A branch arm: a block is walked as a statement list, a single
    /// expression as one statement. Returns the path set surviving the
    /// arm (`None` when every route terminated; an unreachable arm counts
    /// as terminated). A missing arm passes its path set through.
    fn walk_arm(
        &mut self,
        node: Option<&'a OpNode>,
        line: Option<u32>,
        path: &PathSet,
        tail: bool,
    ) -> Option<PathSet> {
        let Some(node) = node else {
            return Some(path.clone());
        };
        let n = node.skip_null();
        match n.name.as_str() {
            "scope" | "leave" | "lineseq" => {
                let stmts = block_statements(n, line);
                self.walk_block(&stmts, path, tail)
            }
            _ => self.walk_block(&[(line, node)], path, tail),
        }
    }

    /// leaveloop: the loop control (iterator / continuation condition) is
    /// not an atom; only branches in the body are collected. The body may
    /// run zero times, so it never terminates the enclosing block and
    /// nothing from inside it is carried past the loop.
    fn walk_loop(&mut self, line: Option<u32>, n: &'a OpNode, path: &PathSet) {
        // A body atom is evaluated once per iteration: with a modification
        // anywhere in the loop, the atoms before, inside and after it may
        // all see different values.
        self.note_mods(n);
        self.walk_loop_kids(line, n, path);
        self.note_mods(n);
    }

    fn walk_loop_kids(&mut self, line: Option<u32>, n: &'a OpNode, path: &PathSet) {
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

    /// Record a value-position expression: decomposed into one outcome per
    /// short-circuit route when possible, otherwise rendered as a whole.
    fn emit_value(&mut self, line: Option<u32>, kind: &str, node: &'a OpNode, path: &PathSet) {
        self.cur_line = line;
        if !is_decomposable(node) {
            self.note_mods(node);
            self.emit(line, kind, vec![render(self.ir, node)], path);
            return;
        }
        for conj in path {
            for r in self.decompose(node, conj.lits.clone(), false, false) {
                let v = value_str(&r);
                self.paths.push(PathOutcome {
                    line,
                    kind: kind.to_string(),
                    exprs: vec![v],
                    when: r.lits,
                    stmts: conj.stmts.clone(),
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
                when: conj.lits.clone(),
                stmts: conj.stmts.clone(),
            });
        }
    }

    /// Split a guard condition into the routes on which it holds and the
    /// routes on which it does not, each extended with the deciding
    /// literal, for every conjunction of the incoming path set.
    fn split(&mut self, cond: &'a OpNode, path: &PathSet, test: Test) -> (Vec<Route>, Vec<Route>) {
        let mut yes = Vec::new();
        let mut no = Vec::new();
        // The condition is decomposed once per incoming conjunction; each
        // evaluation starts from the environment before the condition, so
        // an assignment inside it (`elsif (defined($x = f()))` reached on
        // two routes) bumps the generation once, not once per route.
        let before = self.env.clone();
        let mut after = before.clone();
        for conj in path {
            self.env = before.clone();
            match test {
                Test::Truth => {
                    for r in self.decompose(cond, conj.lits.clone(), false, true) {
                        let atom = self.atom_of(&r);
                        // The expression is true when the deciding atom
                        // has the value `!neg`.
                        if let Some(l) = push_lit(&r.lits, atom.idx, !r.neg ^ atom.neg) {
                            yes.push(Route {
                                lits: l,
                                stmts: conj.stmts.clone(),
                                atom: Some(atom),
                                ..r.clone()
                            });
                        }
                        if let Some(l) = push_lit(&r.lits, atom.idx, r.neg ^ atom.neg) {
                            no.push(Route {
                                lits: l,
                                stmts: conj.stmts.clone(),
                                atom: Some(atom),
                                ..r
                            });
                        }
                    }
                }
                Test::Defined => {
                    let text = logic_canon::display(self.ir, &self.opts, cond);
                    let c = logic_canon::canon(self.ir, &mut self.env, &self.opts, cond);
                    let d = self.intern(
                        Canon {
                            key: format!("defined({})", c.key),
                            neg: false,
                        },
                        format!("defined({})", text),
                        cond.id,
                    );
                    let route = |lits| Route {
                        lits,
                        stmts: conj.stmts.clone(),
                        atom: None,
                        canon: Some(c.clone()),
                        value: text.clone(),
                        node: cond.id,
                        neg: false,
                    };
                    if let Some(l) = push_lit(&conj.lits, d.idx, !d.neg) {
                        yes.push(route(l));
                    }
                    if let Some(l) = push_lit(&conj.lits, d.idx, d.neg) {
                        no.push(route(l));
                    }
                }
            }
            after.join(&self.env);
        }
        self.env = after;
        (yes, no)
    }

    /// Enumerate the short-circuit routes of `node` starting from the
    /// conjunction `lits`. `neg` counts enclosing `not`s; `operand` is
    /// true when the node is an operand of `&&` / `||` / `!` or the
    /// condition of `?:` (its leaves become atoms) and false in value
    /// position (`return` / implicit value / `?:` arms). The routes carry
    /// no statements; callers attach those of the incoming conjunction.
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
                    if let Some(sc) = push_lit(&lr.lits, atom.idx, lhs_decides ^ lr.neg ^ atom.neg) {
                        out.push(Route {
                            lits: sc,
                            stmts: Vec::new(),
                            atom: Some(atom),
                            canon: None,
                            value: lr.value.clone(),
                            node: lr.node,
                            neg: lr.neg ^ neg,
                        });
                    }
                    if let Some(go) = push_lit(&lr.lits, atom.idx, !lhs_decides ^ lr.neg ^ atom.neg) {
                        out.extend(self.decompose(r, go, neg, true));
                    }
                }
                out
            }
            ("dor", [l, r]) => {
                let text = logic_canon::display(self.ir, &self.opts, l);
                let c = logic_canon::canon(self.ir, &mut self.env, &self.opts, l);
                let d = self.intern(
                    Canon {
                        key: format!("defined({})", c.key),
                        neg: false,
                    },
                    format!("defined({})", text),
                    l.id,
                );
                let mut out = Vec::new();
                if let Some(sc) = push_lit(&lits, d.idx, !d.neg) {
                    out.push(Route {
                        lits: sc,
                        stmts: Vec::new(),
                        atom: None,
                        canon: Some(c),
                        value: text,
                        node: l.id,
                        neg,
                    });
                }
                if let Some(go) = push_lit(&lits, d.idx, d.neg) {
                    out.extend(self.decompose(r, go, neg, operand));
                }
                out
            }
            ("cond_expr", [c, t, f]) => {
                let mut out = Vec::new();
                let routes = self.decompose(c, lits, false, true);
                let before = self.env.clone();
                let mut after_t = before.clone();
                for cr in routes {
                    let atom = self.atom_of(&cr);
                    if let Some(go) = push_lit(&cr.lits, atom.idx, !cr.neg ^ atom.neg) {
                        self.env = before.clone();
                        out.extend(self.decompose(t, go, neg, operand));
                        after_t.join(&self.env);
                    }
                    if let Some(go) = push_lit(&cr.lits, atom.idx, cr.neg ^ atom.neg) {
                        self.env = before.clone();
                        out.extend(self.decompose(f, go, neg, operand));
                        after_t.join(&self.env);
                    }
                }
                self.env = after_t;
                out
            }
            _ => {
                let text = render(self.ir, n);
                let atom = if operand {
                    let c = logic_canon::canon(self.ir, &mut self.env, &self.opts, n);
                    let label = logic_canon::display(self.ir, &self.opts, n);
                    Some(self.intern(c, label, n.id))
                } else {
                    self.note_mods(n);
                    None
                };
                vec![Route {
                    lits,
                    stmts: Vec::new(),
                    atom,
                    canon: None,
                    value: text,
                    node: n.id,
                    neg,
                }]
            }
        }
    }

    /// The atom deciding a route, interning the route's value on demand
    /// (an opaque `//` left-hand side whose truth is tested later).
    fn atom_of(&mut self, r: &Route) -> Atom {
        match r.atom {
            Some(a) => a,
            None => {
                let c = r.canon.clone().unwrap_or_else(|| Canon {
                    key: r.value.clone(),
                    neg: false,
                });
                self.intern(c, r.value.clone(), r.node)
            }
        }
    }

    /// The atom with canonical identity `c` (a new column, named by
    /// `text`, when the key is unseen), recording `(node, cur_line)` as
    /// one of its sites. The column takes the polarity of its first site.
    fn intern(&mut self, c: Canon, text: String, node: u32) -> Atom {
        let i = match self.keys.iter().position(|k| *k == c.key) {
            Some(i) => i,
            None => {
                self.conds.push(text.clone());
                self.keys.push(c.key);
                self.col_neg.push(c.neg);
                self.cond_sites.push(Vec::new());
                self.conds.len() - 1
            }
        };
        let neg = c.neg ^ self.col_neg[i];
        let sites = &mut self.cond_sites[i];
        if !sites.iter().any(|s| s.node == node && s.line == self.cur_line) {
            sites.push(CondSite {
                node,
                line: self.cur_line,
                neg,
                text,
            });
        }
        Atom { idx: i, neg }
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
        let l = logic_of(include_str!(
            "../../../../t/golden/5.42-threaded/bool_return.json"
        ));
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
    fn cond_sites_point_at_the_atoms() {
        // sub { my ($a, $b, $c) = @_; return ($a && $b) || !$c }
        let golden = include_str!("../../../../t/golden/5.42-threaded/bool_return.json");
        let ir: SubIr = serde_json::from_str(golden).unwrap();
        let l = analyze_logic(&ir);
        assert_eq!(l.cond_sites.len(), l.conds.len());
        let lines = super::super::node_lines(&ir);
        for (cond, sites) in l.conds.iter().zip(&l.cond_sites) {
            assert_eq!(sites.len(), 1, "{cond} occurs once");
            let site = &sites[0];
            let node = ir.node_by_id(site.node).expect("site node is in the IR");
            assert_eq!(render(&ir, node), *cond);
            assert_eq!(site.text, *cond);
            assert!(!site.neg);
            assert_eq!(site.line, lines[site.node as usize]);
            assert!(site.line.is_some());
        }
    }

    #[test]
    fn ident_defined_assign_is_one_atom() {
        // return 0 unless defined(my $y = f($x)); return 1 if defined($y); 2
        let l = logic_of(include_str!("../../../../t/golden/5.42-threaded/ident_defined_assign.json"));
        assert_eq!(l.conds.len(), 1, "{:?}", l.conds);
        assert_eq!(l.cond_sites[0].len(), 2);
        assert!(l.cond_sites[0].iter().all(|s| !s.neg));
        assert_eq!(l.cond_sites[0][1].text, "defined($y)");
        assert_eq!(l.paths.len(), 2, "the implicit 2 is unreachable");
    }

    #[test]
    fn ident_range_split_negates_second_site() {
        // return "short" if length($s) < 8; return "ok" if length($s) >= 8; "x"
        let golden = include_str!("../../../../t/golden/5.42-threaded/ident_range_split.json");
        let l = logic_of(golden);
        assert_eq!(l.conds, ["length($s) < 8"]);
        let negs: Vec<bool> = l.cond_sites[0].iter().map(|s| s.neg).collect();
        assert_eq!(negs, [false, true]);
        assert_eq!(l.paths.len(), 2);
        assert_eq!(whens(&l.paths[1]), [(0, false)]);
        assert!(l.table.as_ref().unwrap().iter().all(|r| r.path.is_some()));
        let ir: SubIr = serde_json::from_str(golden).unwrap();
        let r = analyze_logic_range(&ir, 3..=4).unwrap();
        assert_eq!(r.logic.conds.len(), 1);
        let kinds: Vec<&str> = r.logic.paths.iter().map(|p| p.kind.as_str()).collect();
        assert_eq!(kinds, ["return", "return"], "no fall-through: the guards are exhaustive");
    }

    #[test]
    fn ident_loop_assign_separates_three_columns() {
        let l = logic_of(include_str!("../../../../t/golden/5.42-threaded/ident_loop_assign.json"));
        assert_eq!(l.conds.len(), 3, "{:?}", l.conds);
    }

    #[test]
    fn ident_elem_assign_separates() {
        let l = logic_of(include_str!("../../../../t/golden/5.42-threaded/ident_elem_assign.json"));
        assert_eq!(l.conds.len(), 2, "{:?}", l.conds);
    }

    #[test]
    fn compound_guard_shares_atoms() {
        // sub { my ($x, $y) = @_; if ($x && !$y) { "a" } elsif ($x) { "b" } else { "c" } }
        let l = logic_of(include_str!(
            "../../../../t/golden/5.42-threaded/compound_guard.json"
        ));
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
    fn arm_statements_are_recorded() {
        // sub { my ($x) = @_; if ($x) { log_it("a"); note(); return 1 } "z" }
        let l = logic_of(include_str!(
            "../../../../t/golden/5.42-threaded/arm_stmts.json"
        ));
        assert_eq!(l.conds, ["$x"]);
        let p = &l.paths;
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].kind, "return");
        let texts: Vec<&str> = p[0].stmts.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["Fixtures::log_it(\"a\")", "Fixtures::note()"]);
        assert_eq!(p[1].exprs, ["\"z\""]);
        assert!(p[1].stmts.is_empty(), "trace is restored after the arm");
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

    fn ir_of(golden: &str) -> SubIr {
        serde_json::from_str(golden).expect("golden fixture deserializes")
    }

    fn range(ir: &SubIr, lo: u32, hi: u32) -> RangeLogic {
        analyze_logic_range(ir, lo..=hi).expect("range has statements")
    }

    fn outcomes(l: &LogicSpec) -> Vec<(Option<u32>, &str, Vec<(usize, bool)>)> {
        l.paths
            .iter()
            .map(|p| (p.line, p.kind.as_str(), whens(p)))
            .collect()
    }

    fn stmt_texts(p: &PathOutcome) -> Vec<&str> {
        p.stmts.iter().map(|s| s.text.as_str()).collect()
    }

    const RANGE_GUARD: &str = include_str!("../../../../t/golden/5.42-threaded/range_guard.json");
    const RANGE_NESTED_IF: &str =
        include_str!("../../../../t/golden/5.42-threaded/range_nested_if.json");
    const RANGE_LOOP_NEXT: &str =
        include_str!("../../../../t/golden/5.42-threaded/range_loop_next.json");

    // sub {                      # 1
    //  my ($x, $y) = @_;         # 2
    //  return 0 unless $x;       # 3
    //  log_it("go");             # 4
    //  if ($y) {                 # 5
    //   note();                  # 6
    //   return 1;                # 7
    //  }                         # 8
    //  done();                   # 9
    //  return 2;                 # 10
    // }                          # 11

    #[test]
    fn range_fallthrough_after_guard() {
        let ir = ir_of(RANGE_GUARD);
        let r = range(&ir, 3, 4);
        assert_eq!((r.lines, r.snapped, r.tail, r.in_block_of), ((3, 4), false, false, None));
        assert_eq!(r.logic.conds, ["$x"]);
        assert_eq!(
            outcomes(&r.logic),
            [
                (Some(3), "return", vec![(0, false)]),
                (Some(4), "fallthrough", vec![(0, true)]),
            ]
        );
        assert_eq!(stmt_texts(&r.logic.paths[1]), ["Fixtures::log_it(\"go\")"]);
        assert_eq!(r.logic.table.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn range_header_snaps_whole_if() {
        let ir = ir_of(RANGE_GUARD);
        // lines 5-8 touch only the `if`; line 8 (`}`) has no statement
        let r = range(&ir, 5, 8);
        assert_eq!((r.lines, r.snapped, r.in_block_of), ((5, 7), true, None));
        assert_eq!(r.logic.conds, ["$y"]);
        assert_eq!(
            outcomes(&r.logic),
            [
                (Some(7), "return", vec![(0, true)]),
                (Some(7), "fallthrough", vec![(0, false)]),
            ]
        );
        assert_eq!(stmt_texts(&r.logic.paths[0]), ["Fixtures::note()"]);
    }

    #[test]
    fn range_inside_if_arm_descends() {
        let ir = ir_of(RANGE_GUARD);
        let r = range(&ir, 6, 7);
        assert_eq!(r.in_block_of, Some(27), "the `if` statement");
        assert_eq!(r.stmt_ids, [33, 39]);
        assert!(r.logic.conds.is_empty());
        assert_eq!(outcomes(&r.logic), [(Some(7), "return", vec![])]);
    }

    #[test]
    fn range_tail_keeps_implicit_value() {
        let ir = ir_of(RANGE_NESTED_IF);
        // `finish();` is the sub's last statement
        let r = range(&ir, 9, 10);
        assert_eq!((r.lines, r.snapped, r.tail), ((10, 10), true, true));
        assert_eq!(outcomes(&r.logic), [(Some(10), "implicit", vec![])]);
        assert_eq!(r.logic.paths[0].exprs, ["Fixtures::finish()"]);
    }

    #[test]
    fn range_whole_body_matches_analyze_logic() {
        for golden in [RANGE_GUARD, RANGE_NESTED_IF] {
            let ir = ir_of(golden);
            let r = range(&ir, 1, 1000);
            assert!(r.tail);
            assert_eq!(r.logic, analyze_logic(&ir));
        }
    }

    // sub {                        # 1
    //  my (@items) = @_;           # 2
    //  my $n = 0;                  # 3
    //  for my $it (@items) {       # 4
    //   next unless $it;           # 5
    //   last if $it eq "stop";     # 6
    //   $n++;                      # 7
    //  }                           # 8
    //  return $n;                  # 9
    // }                            # 10

    #[test]
    fn range_loop_body_control_is_outcome() {
        let ir = ir_of(RANGE_LOOP_NEXT);
        let r = range(&ir, 5, 7);
        assert_eq!(r.in_block_of, Some(16), "the loop");
        assert_eq!(r.logic.conds, ["$it", "$it eq \"stop\""]);
        assert_eq!(
            outcomes(&r.logic),
            [
                (Some(5), "next", vec![(0, false)]),
                (Some(6), "last", vec![(0, true), (1, true)]),
                (Some(7), "fallthrough", vec![(0, true), (1, false)]),
            ]
        );
        assert_eq!(stmt_texts(&r.logic.paths[2]), ["++$n"]);
        // whole-sub mode keeps loop control as plain statements
        let whole = analyze_logic(&ir);
        assert_eq!(outcomes(&whole), [(Some(9), "return", vec![])]);
    }

    #[test]
    fn range_without_statements_is_an_error() {
        let ir = ir_of(RANGE_GUARD);
        assert_eq!(
            analyze_logic_range(&ir, 11..=12),
            Err(RangeError::NoStatements { lines: (11, 12) })
        );
        assert_eq!(
            analyze_logic_range(&ir, 1..=1),
            Err(RangeError::NoStatements { lines: (1, 1) }),
            "`sub {{` has no statement"
        );
    }

    #[test]
    fn fallthrough_branch_carries_arm_statements() {
        // if ($a) { log_it("a"); die "bad" if $b; note() } else { done() } finish()
        let l = analyze_logic(&ir_of(RANGE_NESTED_IF));
        assert_eq!(l.conds, ["$a", "$b"]);
        assert_eq!(
            outcomes(&l),
            [
                (Some(5), "die", vec![(0, true), (1, true)]),
                (Some(10), "implicit", vec![(0, true), (1, false)]),
                (Some(10), "implicit", vec![(0, false)]),
            ]
        );
        assert_eq!(stmt_texts(&l.paths[1]), ["Fixtures::log_it(\"a\")", "Fixtures::note()"]);
        assert_eq!(stmt_texts(&l.paths[2]), ["Fixtures::done()"]);
    }
}
