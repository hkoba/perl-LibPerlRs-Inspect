//! Abstract (symbolic) execution of a sub body over [`SubIr`], in terms
//! of the sub's `@_`.
//!
//! The executor walks the OpNode tree statement by statement. Values are
//! [`Sym`]s: constants are evaluated on the spot, anything that depends on
//! the arguments stays symbolic (`$_[0]`, `@_[1..]`, `$args{foo}`). At a
//! condition it cannot decide it runs both arms and joins their states
//! right after the compound statement; an arm that dies is dropped and the
//! condition that avoids it becomes a precondition. The result is a
//! [`Summary`] of how the arguments are read, unpacked, checked and
//! passed on, from which `argspec` derives the argument spec.
//!
//! This module depends only on `ir` and `render`, so that it can move to
//! a crate of its own later.

mod builtins;
pub mod env;
mod exec;
pub mod summary;
pub mod value;

use std::sync::Arc;

use crate::ir::SubIr;

pub use env::{Env, Frame, JoinLimits, arity_offset};
pub use summary::{Assign, CallEvent, ForwardEvent, Precond, Read, ReadKind, Summary};
pub use value::{Cond, Constraint, DefaultWhen, HashSeg, Key, Norm, Sym};

/// Bounds on one execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Total number of evaluated ops (inlined callees included).
    pub steps: u32,
    /// Most alternatives a joined value keeps before it becomes unknown.
    pub join_width: u32,
    /// Deepest chain of inlined calls.
    pub inline_depth: u32,
    /// Most iterations of a loop over concrete values.
    pub loop_iters: u32,
    /// Most keys a table lookup is split into.
    pub table_split: u32,
    /// Most nodes of a joined or assigned value ([`Sym::size`]) before it
    /// becomes unknown.
    pub value_size: u32,
    /// Total work: the size of the state copied at each fork (in value
    /// nodes, see [`Sym::size`]). Bounds the time and memory of one
    /// execution, which `steps` alone does not.
    pub work: u64,
}

impl Limits {
    pub fn join(&self) -> JoinLimits {
        JoinLimits {
            width: self.join_width,
            size: self.value_size,
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            steps: 20_000,
            join_width: 8,
            inline_depth: 4,
            loop_iters: 64,
            table_split: 16,
            value_size: 512,
            work: 2_000_000,
        }
    }
}

/// Supplies what the executor needs beyond the analyzed sub: callee
/// bodies to inline and values of captured variables.
pub trait IrSource {
    /// The body of `callee` (as named at the call site, unqualified names
    /// in `caller_pkg`), if it should be inlined.
    fn sub_ir(&self, callee: &str, caller_pkg: &str) -> Option<Arc<SubIr>>;
    /// The name `callee` is defined under (an imported name resolved to
    /// its home package).
    fn canonical_name(&self, callee: &str, caller_pkg: &str) -> Option<String>;
    /// The value of a captured (OUTER) pad entry of `callee`.
    fn outer_value(&self, callee: &str, pad_ix: u32) -> Option<Sym>;
}

/// An [`IrSource`] that supplies nothing (no inlining).
pub struct NoSource;

impl IrSource for NoSource {
    fn sub_ir(&self, _: &str, _: &str) -> Option<Arc<SubIr>> {
        None
    }
    fn canonical_name(&self, _: &str, _: &str) -> Option<String> {
        None
    }
    fn outer_value(&self, _: &str, _: u32) -> Option<Sym> {
        None
    }
}

/// Execute the body of `ir` symbolically.
pub fn exec_sub(ir: &SubIr, src: &dyn IrSource, limits: &Limits) -> Summary {
    exec::Exec::new(ir, src, limits).run()
}
