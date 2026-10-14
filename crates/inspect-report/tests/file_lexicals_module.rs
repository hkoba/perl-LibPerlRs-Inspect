//! U6: `file_lexicals` of files compiled as modules, i.e. by `require`,
//! with [`inspect_report::prepare`] (every required file is kept): from
//! `BEGIN { require FILE }` during perl_parse, and from a `require` run
//! with eval_pv after it. A file starting with `#line` is still found by
//! the path it was required by.
//!
//! One embedded interpreter per test process, as in api.rs.

mod common;

use common::*;
use libperl_rs::Perl;

#[test]
fn module_file_lexicals() {
    let ws = Ws::new("modlex");
    let cold = ws.module("Cold", "");
    let warm = ws.module("Warm", "");
    let relined = ws.module("Relined", "#line 1 \"elsewhere.pm\"\n");

    let inc = format!("-I{}", ws.dir().display());
    let code = "BEGIN { require $ARGV[0]; require $ARGV[1] }";
    let args: Vec<String> = [
        "inspect-report-test",
        &inc,
        "-e",
        code,
        "--",
        &cold,
        &relined,
    ]
    .map(String::from)
    .into();

    let mut perl = Perl::new();
    inspect_report::prepare(&perl);
    assert_eq!(perl.parse(&args, &envp()), 0, "parse failed");
    require_at_runtime(&perl, &warm);

    // report() is one-shot for the symbol table (B::Deparse); later calls
    // are fine for file_lexicals, which do not depend on it
    let report = |file: &str| inspect_report::report(&perl, file, &Default::default());
    assert_file_lexicals(&report(&warm), &warm, "Lx::Warm", 0);
    assert_file_lexicals(&report(&cold), &cold, "Lx::Cold", 0);
    // `#line 1` takes the place of line 1, so the lines stay the same
    assert_file_lexicals(&report(&relined), &relined, "Lx::Relined", 0);
    // every required file is kept, the dependency too
    assert!(report(&ws.path("Lx/Base.pm"))["file_lexicals"].is_array());

    // a file nobody compiled
    assert_not_captured(&report("/no/such/File.pm"));
}
