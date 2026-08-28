//! inspect-core — Perl OP ツリーの所有型 IR と解析パス。
//!
//! libperl には依存しない。IR (SubIr) は inspect-capture が生成し、
//! ここの解析パスはインタプリタ無しで `cargo test` できる。

pub mod dump;
pub mod ir;
pub mod passes;
pub mod render;
