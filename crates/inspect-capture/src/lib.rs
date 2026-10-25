//! inspect-capture — FFI layer that copies a live Perl OP tree into
//! inspect-core::ir::SubIr (owned-type IR).
//!
//! Assumptions: Perl 5.42 / ithreads+multiplicity build (system perl).
//! Analysis completes within a single XS call; no raw pointers remain in the IR.

mod begins;
mod capture;
pub mod dbsrc;
pub mod filescope;
pub mod packages;
mod raw;
pub mod reqtrace;
pub mod subs;

pub use begins::{
    FileBegins, FileUse, enable_begin_capture, file_begins, saved_begins, set_begin_capture,
};
pub use capture::{
    Captured, capture_main, capture_main_full, capture_sub, capture_sub_full, cv_lines,
    exec_op_names,
};
