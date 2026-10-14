//! U6: `file_lexicals` limited to named files
//! ([`inspect_report::FileLexicals::Files`]): only the target is kept,
//! not the modules it loads; and a target named after parse
//! ([`inspect_report::want_file_lexicals`]) is kept when a later
//! `require` compiles it.
//!
//! One embedded interpreter per test process, as in api.rs.

mod common;

use common::*;
use inspect_report::{FileLexicals, PrepareOpts};
use libperl_rs::Perl;

#[test]
fn targeted_file_lexicals() {
    let ws = Ws::new("modlex-targeted");
    let cold = ws.module("Cold", "");
    let warm = ws.module("Warm", "");
    let other = ws.module("Other", "");

    let inc = format!("-I{}", ws.dir().display());
    let args: Vec<String> = [
        "inspect-report-test",
        &inc,
        "-e",
        "BEGIN { require $ARGV[0] }",
        "--",
        &cold,
    ]
    .map(String::from)
    .into();

    let mut perl = Perl::new();
    let mut opts = PrepareOpts::default();
    opts.file_lexicals = FileLexicals::Files(vec![cold.clone()]);
    inspect_report::prepare_with(&perl, &opts);
    assert_eq!(perl.parse(&args, &envp()), 0, "parse failed");

    // named after parse, then required
    inspect_report::want_file_lexicals(&warm);
    require_at_runtime(&perl, &warm);
    // required after parse, never named
    require_at_runtime(&perl, &other);

    let report = |file: &str| inspect_report::report(&perl, file, &Default::default());
    assert_file_lexicals(&report(&cold), &cold, "Lx::Cold", 0);
    assert_file_lexicals(&report(&warm), &warm, "Lx::Warm", 0);
    // loaded by the targets (use Lx::Base) during the same parse: not kept
    assert_not_captured(&report(&ws.path("Lx/Base.pm")));
    assert_not_captured(&report(&other));
}
