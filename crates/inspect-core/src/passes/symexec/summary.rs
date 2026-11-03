//! What one symbolic execution of a sub observed.

use super::env::Env;
use super::value::{Cond, Sym};

/// The result of [`super::exec_sub`].
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// State at the end of the sub: the fall-through path joined with the
    /// state at every `return`.
    pub env: Env,
    /// Reads of argument-derived values, in execution order.
    pub reads: Vec<Read>,
    /// The first assignment of an argument-derived value to each pad
    /// entry of the analyzed sub, in execution order.
    pub assigns: Vec<Assign>,
    /// Conditions every successful call satisfies (the condition that
    /// avoids a dying arm), or that a non-returning path satisfies
    /// (`enforced` = false: the other arm returns early).
    pub preconds: Vec<Precond>,
    /// Writes to the caller's variables (`$_[0] = …`), by pad name.
    pub writes_to_caller: Vec<(String, Sym)>,
    pub calls: Vec<CallEvent>,
    /// Calls that pass the remaining arguments on.
    pub forwards: Vec<ForwardEvent>,
    /// Values returned (explicit `return` and the value of the last
    /// statement).
    pub returns: Vec<Sym>,
    /// Ops the executor did not model, with their line.
    pub unknowns: Vec<(String, Option<u32>)>,
    pub notes: Vec<String>,
    /// The step budget ran out before the end of the body.
    pub exhausted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Read {
    /// Event sequence number (shared with [`Assign::seq`]).
    pub seq: u32,
    pub sym: Sym,
    pub line: Option<u32>,
    /// The read is under a branch condition or in a loop body.
    pub conditional: bool,
    pub kind: ReadKind,
    /// The variable the value was read through (`%args`, `$opts`).
    pub via: Option<String>,
    /// Source text of the access (`$_[0]`).
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReadKind {
    /// An array element (`$_[i]`, `shift` is not a read)
    Elem,
    /// A hash element (`$args{k}`, `$opts->{k}`)
    Key,
    /// `exists $args{k}`
    Exists,
    /// A value with a fallback (`$args{k} // d`, `shift // d`)
    Default(Sym),
    /// The loop variable over a list (`foreach (@_)`)
    Iter,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assign {
    pub seq: u32,
    pub targ: u64,
    pub sym: Sym,
    pub line: Option<u32>,
    pub conditional: bool,
    /// shift | pop | splice | unpack | elem | expr
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Precond {
    pub cond: Cond,
    pub line: Option<u32>,
    /// The other arm dies (rather than returns).
    pub enforced: bool,
    /// The condition is itself under a branch or in a loop body.
    pub conditional: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallEvent {
    pub callee: String,
    pub args: Vec<Sym>,
    pub line: Option<u32>,
    pub method: bool,
    pub inlined: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForwardEvent {
    pub callee: String,
    pub line: Option<u32>,
    /// rest | all | goto | amper
    pub kind: &'static str,
    /// Index of the first original argument passed on.
    pub from: u32,
    pub method: bool,
}
