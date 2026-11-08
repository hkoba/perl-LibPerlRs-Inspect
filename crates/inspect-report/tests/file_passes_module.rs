//! U9: `file_calls` of a file compiled as a module (`require`), named
//! with `FileLexicals::Files`, and the error for a file whose top level
//! was not kept.
//!
//! One embedded interpreter per test process, as in api.rs.

#[allow(dead_code)]
mod common;

use common::*;
use inspect_report::{FileLexicals, Pass, PrepareOpts, ReportOpts};
use libperl_rs::Perl;
use serde_json::json;

const MODULE: &str = "\
package Lx::Top;
use Lx::Base;
my $app = Lx::Top->new(port => 80);
setup($app, 1);
my $cb = sub { my ($req) = @_; $req };
sub new { my ($class, %a) = @_; bless {%a}, $class }
sub setup { my ($app, $n) = @_; $n }
1;
";

#[test]
fn module_file_calls() {
    let ws = Ws::new("modcalls");
    let top = ws.path("Lx/Top.pm");
    std::fs::write(&top, MODULE).expect("write");

    let inc = format!("-I{}", ws.dir().display());
    let args: Vec<String> = ["inspect-report-test", &inc, "-e", "BEGIN { require $ARGV[0] }", "--", &top]
        .map(String::from)
        .into();
    let mut perl = Perl::new();
    let mut prep = PrepareOpts::default();
    prep.file_lexicals = FileLexicals::Files(vec![top.clone()]);
    inspect_report::prepare_with(&perl, &prep);
    assert_eq!(perl.parse(&args, &envp()), 0, "parse failed");

    let mut opts = ReportOpts::default();
    opts.passes = vec![Pass::Calls, Pass::Lints];
    let v = inspect_report::report(&perl, &top, &opts);
    assert!(v.get("file_calls_error").is_none(), "{}", v["file_calls_error"]);
    let calls = v["file_calls"].as_array().expect("file_calls");
    let callees: Vec<_> = calls.iter().map(|c| c["callee"].clone()).collect();
    assert_eq!(
        callees,
        [
            json!({ "method": { "name": "new", "invocant": { "class": "Lx::Top" } } }),
            json!({ "named": { "name": "Lx::Top::setup" } }),
        ]
    );
    assert_eq!(calls[0]["line"], 3);
    assert_eq!(calls[1]["argc"], json!({ "min": 2, "max": 2 }));
    assert!(v["file_lints"].is_object(), "{}", v["file_lints"]);
    let anon = v["file_anon_subs"].as_array().expect("file_anon_subs");
    assert_eq!(anon[0]["lexicals"][0]["name"], "$req");

    // Lx/Base.pm was compiled too, but only the named file keeps its tree
    let base = inspect_report::report(&perl, &ws.path("Lx/Base.pm"), &opts);
    assert!(base.get("file_calls").is_none());
    assert!(base["file_calls_error"].is_string(), "{base}");
}
