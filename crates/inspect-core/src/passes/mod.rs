//! Analysis passes. All of them are pure Rust operating on SubIr (the owned IR).
//!
//! Besides the passes themselves this module exposes the statement-level
//! view of a sub body that they (and GUI front ends) share: the statement
//! list of a block ([`statements`], [`block_statements`]), the nested
//! blocks of a compound statement ([`nested_blocks`]), the statement tree
//! with source-line spans ([`statement_tree`]) and node ↔ line maps
//! ([`node_lines`], [`line_index`]).

pub mod argspec;
pub mod deps;
pub mod lints;
pub mod logic;
pub mod retspec;
pub mod types;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ir::{OpClass, OpDetail, OpNode, SubIr};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub args: argspec::ArgSpec,
    pub returns: retspec::RetSpec,
    pub logic: logic::LogicSpec,
    pub types: types::TypesSpec,
    pub lints: lints::LintsSpec,
}

pub fn analyze(ir: &SubIr) -> AnalysisReport {
    AnalysisReport {
        args: argspec::analyze_args(ir),
        returns: retspec::analyze_returns(ir),
        logic: logic::analyze_logic(ir),
        types: types::analyze_types(ir),
        lints: lints::analyze_lints(ir),
    }
}

/// A statement with the line of the COP that precedes it.
pub type Stmt<'a> = (Option<u32>, &'a OpNode);

/// The block op holding the statement list of the body: `leavesub` /
/// `leavesublv` / `leaveeval` → their `lineseq` kid; `leave` (a main
/// program captured with `capture_main`) is the block itself.
pub fn body(ir: &SubIr) -> Option<&OpNode> {
    let root = ir.root.skip_null();
    match root.name.as_str() {
        "leave" => Some(root),
        _ => root.kids.first().map(|k| k.skip_null()),
    }
}

/// Whether the body's last expression is its value (`leavesub` /
/// `leavesublv` / `leaveeval`); a main program (`leave`) has none.
pub fn has_implicit_value(ir: &SubIr) -> bool {
    matches!(
        ir.root.skip_null().name.as_str(),
        "leavesub" | "leavesublv" | "leaveeval"
    )
}

/// Statement list of the sub body: split the children of the body block
/// at COPs into a sequence of (line number, statement op).
pub fn statements(ir: &SubIr) -> Vec<Stmt<'_>> {
    match body(ir) {
        Some(b) => block_statements(b, None),
        None => Vec::new(),
    }
}

/// Split the kids of a block op at COPs into (line, statement) pairs.
/// `enter` / `entertry` / `unstack` do not count as statements. `inherited`
/// is the line in force before the block (used until its first COP).
pub fn block_statements(n: &OpNode, inherited: Option<u32>) -> Vec<Stmt<'_>> {
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

/// A statement list nested in a compound statement.
#[derive(Debug, Clone)]
pub struct Block<'a> {
    /// What the block is to its statement.
    pub role: BlockRole,
    pub stmts: Vec<Stmt<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockRole {
    /// True arm of `if` / `?:`, or the arm of `STMT if COND` / `and`.
    Then,
    /// False arm of `if` / `?:`, or the arm of `unless` / `or` / `//`.
    Else,
    /// Body of a loop (may run zero or many times).
    LoopBody,
    /// Bare block or `do BLOCK`.
    Block,
    /// Body of `eval BLOCK`.
    Eval,
}

impl BlockRole {
    /// Whether the block's last statement is the value of the compound
    /// statement when that statement is in tail position.
    pub fn carries_tail(self) -> bool {
        matches!(self, BlockRole::Then | BlockRole::Else | BlockRole::Block)
    }
}

/// An arm or body as a statement list: a block op is split at its COPs,
/// a single expression is one statement.
fn arm_statements<'a>(node: &'a OpNode, line: Option<u32>) -> Vec<Stmt<'a>> {
    let n = node.skip_null();
    match n.name.as_str() {
        "scope" | "leave" | "lineseq" => block_statements(n, line),
        _ => vec![(line, node)],
    }
}

/// The statement lists nested in a compound statement (mirrors the
/// dispatch of the logic pass). A simple statement has none.
pub fn nested_blocks<'a>(node: &'a OpNode, line: Option<u32>) -> Vec<Block<'a>> {
    let n = node.skip_null();
    let mut out = Vec::new();
    match n.name.as_str() {
        "cond_expr" => {
            if let Some(t) = n.kids.get(1) {
                out.push(Block {
                    role: BlockRole::Then,
                    stmts: arm_statements(t, line),
                });
            }
            if let Some(f) = n.kids.get(2) {
                out.push(Block {
                    role: BlockRole::Else,
                    stmts: arm_statements(f, line),
                });
            }
        }
        "and" | "or" | "dor" => {
            if let Some(arm) = n.kids.get(1) {
                let role = if n.name == "and" {
                    BlockRole::Then
                } else {
                    BlockRole::Else
                };
                out.push(Block {
                    role,
                    stmts: arm_statements(arm, line),
                });
            }
        }
        "scope" | "leave" | "lineseq" => out.push(Block {
            role: BlockRole::Block,
            stmts: block_statements(n, line),
        }),
        "leavetry" => out.push(Block {
            role: BlockRole::Eval,
            stmts: block_statements(n, line),
        }),
        "leaveloop" => {
            for k in &n.kids {
                let s = k.skip_null();
                match s.name.as_str() {
                    "enteriter" | "enterloop" => {}
                    "and" | "or" => {
                        if let Some(b) = s.kids.get(1) {
                            out.push(Block {
                                role: BlockRole::LoopBody,
                                stmts: arm_statements(b, line),
                            });
                        }
                    }
                    _ => out.push(Block {
                        role: BlockRole::LoopBody,
                        stmts: arm_statements(k, line),
                    }),
                }
            }
        }
        _ => {}
    }
    out
}

/// Smallest and largest COP line inside `node` (its own statement line
/// `line` included).
pub fn line_span(node: &OpNode, line: Option<u32>) -> Option<(u32, u32)> {
    fn rec(n: &OpNode, acc: &mut Option<(u32, u32)>) {
        if let OpDetail::Cop { line, .. } = &n.detail {
            widen(acc, *line);
        }
        for k in &n.kids {
            rec(k, acc);
        }
    }
    fn widen(acc: &mut Option<(u32, u32)>, l: u32) {
        *acc = Some(match *acc {
            Some((lo, hi)) => (lo.min(l), hi.max(l)),
            None => (l, l),
        });
    }
    let mut acc = None;
    if let Some(l) = line {
        widen(&mut acc, l);
    }
    rec(node, &mut acc);
    acc
}

/// One statement of the statement tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StmtSpan {
    /// Node id of the statement op.
    pub id: u32,
    /// Op name of the statement (`skip_null`ed), e.g. `cond_expr`,
    /// `entersub`, `leaveloop`.
    pub op: String,
    /// Line of the COP preceding the statement.
    pub line: Option<u32>,
    /// Smallest and largest source line touched by the statement.
    pub span: Option<(u32, u32)>,
    /// Nested statement lists (arms, bodies) of a compound statement.
    pub blocks: Vec<StmtBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StmtBlock {
    pub role: BlockRole,
    pub span: Option<(u32, u32)>,
    pub stmts: Vec<StmtSpan>,
}

fn stmt_spans(stmts: &[Stmt<'_>]) -> Vec<StmtSpan> {
    stmts
        .iter()
        .map(|(line, node)| StmtSpan {
            id: node.id,
            op: node.skip_null().name.clone(),
            line: *line,
            span: line_span(node, *line),
            blocks: nested_blocks(node, *line)
                .into_iter()
                .map(|b| {
                    let stmts = stmt_spans(&b.stmts);
                    let span = stmts
                        .iter()
                        .filter_map(|s| s.span)
                        .fold(None, |acc, (lo, hi)| {
                            Some(match acc {
                                Some((a, b)) => (lo.min(a), hi.max(b)),
                                None => (lo, hi),
                            })
                        });
                    StmtBlock {
                        role: b.role,
                        span,
                        stmts,
                    }
                })
                .collect(),
        })
        .collect()
}

/// The nested statement structure of the sub body with source-line spans
/// (for selection snapping in editors; see `logic::analyze_logic_range`).
pub fn statement_tree(ir: &SubIr) -> Vec<StmtSpan> {
    stmt_spans(&statements(ir))
}

/// Visit every node in pre-order while tracking the current COP line number
pub fn walk_with_lines<'a>(ir: &'a SubIr, mut f: impl FnMut(&'a OpNode, Option<u32>)) {
    fn rec<'a>(
        node: &'a OpNode,
        line: &mut Option<u32>,
        f: &mut impl FnMut(&'a OpNode, Option<u32>),
    ) {
        if let OpDetail::Cop { line: l, .. } = &node.detail {
            *line = Some(*l);
        }
        f(node, *line);
        for k in &node.kids {
            rec(k, line, f);
        }
    }
    let mut line = None;
    rec(&ir.root, &mut line, &mut f);
}

/// Source line of every node, indexed by node id (the line of the COP
/// most recently passed in pre-order; `None` before the first COP).
pub fn node_lines(ir: &SubIr) -> Vec<Option<u32>> {
    let mut out = Vec::new();
    walk_with_lines(ir, |n, line| {
        let ix = n.id as usize;
        if out.len() <= ix {
            out.resize(ix + 1, None);
        }
        out[ix] = line;
    });
    out
}

/// Node ids per source line (pre-order within a line).
pub fn line_index(ir: &SubIr) -> BTreeMap<u32, Vec<u32>> {
    let mut out: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    walk_with_lines(ir, |n, line| {
        if let Some(l) = line {
            out.entry(l).or_default().push(n.id);
        }
    });
    out
}
