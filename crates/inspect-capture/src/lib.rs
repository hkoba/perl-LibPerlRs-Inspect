//! inspect-capture — 生きた Perl の OP ツリーを inspect-core::ir::SubIr
//! (所有型 IR) へ写し取る FFI 層。
//!
//! 前提: Perl 5.42 / ithreads+multiplicity ビルド (システム perl)。
//! 解析は 1 回の XS 呼び出し内で完結し、生ポインタは IR に残さない。

mod begins;
mod capture;
mod raw;

pub use begins::{FileBegins, FileUse, enable_begin_capture, file_begins};
pub use capture::{capture_sub, exec_op_names};
