//! inspect-core — owned IR of the Perl OP tree and the analysis passes over it.
//!
//! Does not depend on libperl. The IR (SubIr) is produced by inspect-capture,
//! and the analysis passes here can be `cargo test`ed without an interpreter.

pub mod dump;
pub mod ir;
pub mod passes;
pub mod render;
