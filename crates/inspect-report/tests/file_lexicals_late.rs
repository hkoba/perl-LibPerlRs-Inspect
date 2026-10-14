//! U6: an interpreter prepared with
//! [`inspect_report::FileLexicals::MainProgram`] keeps no required file
//! (e.g. one that preloads modules before its target is known); a
//! target named after parse is still kept by a later `require`.
//!
//! One embedded interpreter per test process, as in api.rs.

mod common;

use common::*;
use inspect_report::{FileLexicals, PrepareOpts};
use libperl_rs::Perl;

#[test]
fn late_target_file_lexicals() {
    let ws = Ws::new("modlex-late");
    let target = ws.module("Target", "");

    let inc = format!("-I{}", ws.dir().display());
    let args: Vec<String> = [
        "inspect-report-test",
        &inc,
        "-e",
        "BEGIN { require Lx::Base }",
    ]
    .map(String::from)
    .into();

    let mut perl = Perl::new();
    let mut opts = PrepareOpts::default();
    opts.file_lexicals = FileLexicals::MainProgram;
    inspect_report::prepare_with(&perl, &opts);
    assert_eq!(perl.parse(&args, &envp()), 0, "parse failed");

    inspect_report::want_file_lexicals(&target);
    require_at_runtime(&perl, &target);

    let report = |file: &str| inspect_report::report(&perl, file, &Default::default());
    assert_file_lexicals(&report(&target), &target, "Lx::Target", 0);
    // preloaded during parse: not kept
    assert_not_captured(&report(&ws.path("Lx/Base.pm")));
    // the main program is always available
    assert_eq!(report("-e")["file_lexicals"], serde_json::json!([]));
}
