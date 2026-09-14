//! inspect-capture — FFI layer that copies a live Perl OP tree into
//! inspect-core::ir::SubIr (owned-type IR).
//!
//! Assumptions: Perl 5.42 / ithreads+multiplicity build (system perl).
//! Analysis completes within a single XS call; no raw pointers remain in the IR.

mod begins;
mod capture;
mod raw;

pub use begins::{FileBegins, FileUse, enable_begin_capture, file_begins};
pub use capture::{capture_sub, exec_op_names};
