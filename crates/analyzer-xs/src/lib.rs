//! OpTree::Analyzer — XS glue crate.
//!
//! Loaded from Perl via XSLoader as `auto/OpTree/Analyzer/Analyzer.so`.
//! All results cross the boundary as JSON strings (decoded with
//! JSON::PP on the Perl side) while the schema is still in flux.

use libperl_rs::{Perl, SV, xs_boot, xs_sub};

/// `OpTree::Analyzer::op_names_json($coderef)` — op names of the sub's
/// execution-order chain (CvSTART → op_next → …), as a JSON array.
#[xs_sub]
fn op_names_json(code: *mut SV) -> Result<String, String> {
    let names = analyzer_capture::exec_op_names(code)?;
    serde_json::to_string(&names).map_err(|e| e.to_string())
}

/// `OpTree::Analyzer::capture_json($coderef)` — full owned IR (SubIr)
/// of the sub's OP tree, as JSON.
#[xs_sub]
fn capture_json(my_perl: &Perl, code: *mut SV) -> Result<String, String> {
    let ir = analyzer_capture::capture_sub(my_perl, code)?;
    serde_json::to_string(&ir).map_err(|e| e.to_string())
}

/// `OpTree::Analyzer::dump_optree($coderef)` — human-readable tree dump
/// for debugging.
#[xs_sub]
fn dump_optree(my_perl: &Perl, code: *mut SV) -> Result<String, String> {
    let ir = analyzer_capture::capture_sub(my_perl, code)?;
    Ok(analyzer_core::dump::dump(&ir))
}

/// `OpTree::Analyzer::analyze_json($coderef)` — analysis report
/// (argument spec, return/exception spec; grows per milestone), as JSON.
#[xs_sub]
fn analyze_json(my_perl: &Perl, code: *mut SV) -> Result<String, String> {
    let ir = analyzer_capture::capture_sub(my_perl, code)?;
    let report = analyzer_core::passes::analyze(&ir);
    serde_json::to_string(&report).map_err(|e| e.to_string())
}

xs_boot! {
    package = "OpTree::Analyzer";
    subs = [op_names_json, capture_json, dump_optree, analyze_json];
}
