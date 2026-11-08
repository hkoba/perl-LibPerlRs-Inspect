//! U9 / U10 / U8-4 in script mode: `ReportOpts::passes` on subs, the top
//! level (`file_calls`, ...) and anonymous subs (`anon_subs`), call
//! order within a line (`line_seq`), and `ArgsAnalysis::Preamble`.
//!
//! One embedded interpreter per test process, as in api.rs.

use inspect_report::{ArgsAnalysis, Pass, ReportOpts};
use libperl_rs::Perl;
use serde_json::{Value, json};

const SCRIPT: &str = "\
package Foo;
sub new { my ($class, %a) = @_; bless {%a}, $class }
sub run { my ($self, $cb) = @_; $self->go->go; $cb->(1) }
sub go { $_[0] }
package main;
sub main { my (@argv) = @_; Foo->new(k => 1)->run(sub { my ($req) = @_; my $f = sub { my ($y) = @_; $y }; $f->($req) }) }
my $handler = sub { my ($env, $opt) = @_; main(@ARGV) };
main(@ARGV);
Foo->new(k => 1);
";

fn callees(calls: &Value) -> Vec<Value> {
    calls
        .as_array()
        .unwrap_or_else(|| panic!("calls array: {calls}"))
        .iter()
        .map(|c| c["callee"].clone())
        .collect()
}

fn names(lex: &Value) -> Vec<&str> {
    lex.as_array()
        .expect("lexicals")
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect()
}

#[test]
fn passes_top_level_and_anon_subs() {
    let args: Vec<String> = ["inspect-report-test", "-e", SCRIPT].map(String::from).into();
    let envp: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    let mut perl = Perl::new();
    inspect_report::prepare(&perl);
    assert_eq!(perl.parse(&args, &envp), 0, "parse failed");

    let mut opts = ReportOpts::default();
    opts.passes = vec![Pass::Calls];
    let v = inspect_report::report(&perl, "-e", &opts);

    // only the selected pass, next to args
    let main = &v["packages"]["main"]["subs"]["main"];
    for k in ["calls", "args", "lexicals"] {
        assert!(main.get(k).is_some(), "{k} missing: {main}");
    }
    for k in ["logic", "types", "lints", "returns", "report"] {
        assert!(main.get(k).is_none(), "{k} present: {main}");
    }
    assert!(v.get("file_logic").is_none() && v.get("file_lints").is_none());

    // the top level's calls
    assert_eq!(
        callees(&v["file_calls"]),
        [
            json!({ "named": { "name": "main::main" } }),
            json!({ "method": { "name": "new", "invocant": { "class": "Foo" } } }),
        ]
    );
    assert_eq!(v["file_calls"][0]["line"], 8);
    assert_eq!(v["file_calls"][1]["argc"], json!({ "min": 2, "max": 2 }));

    // line_seq follows the source: `$self->go->go` is go(go($self)) in the tree
    let run = &v["packages"]["Foo"]["subs"]["run"]["calls"];
    let seq: Vec<(String, u64)> = run
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["callee"].to_string(), c["line_seq"].as_u64().unwrap()))
        .collect();
    let inner = run.as_array().unwrap().iter().find(|c| c["callee"]["method"]["invocant"]["self_var"] == "$self");
    assert_eq!(inner.expect("inner go")["line_seq"], 0, "{seq:?}");
    assert_eq!(run[0]["line_seq"], 1, "outer go comes second: {seq:?}");

    // anonymous subs of a named sub, the nested one too
    let anon = main["anon_subs"].as_array().expect("anon_subs");
    assert_eq!(anon.len(), 2, "{anon:?}");
    assert_eq!(names(&anon[0]["lexicals"]), ["$req", "$f"]);
    assert_eq!(anon[0]["args"]["params"][0]["name"], "$req");
    assert_eq!(anon[0]["lines"], json!([6, 6]));
    assert_eq!(callees(&anon[0]["calls"]), [json!({ "dynamic": "$f" })]);
    assert_eq!(names(&anon[1]["lexicals"]), ["$y"]);

    // and of the top level
    let fa = v["file_anon_subs"].as_array().expect("file_anon_subs");
    assert_eq!(fa.len(), 1);
    assert_eq!(names(&fa[0]["lexicals"]), ["$env", "$opt"]);
    assert_eq!(fa[0]["args"]["max_arity"], 2);
    assert_eq!(callees(&fa[0]["calls"]), [json!({ "named": { "name": "main::main" } })]);

    // the preamble-only argument analysis
    let mut opts = ReportOpts::default();
    opts.args_analysis = ArgsAnalysis::Preamble;
    let v = inspect_report::report(&perl, "-e", &opts);
    let new = &v["packages"]["Foo"]["subs"]["new"]["args"];
    assert_eq!(new["style"], "unpack");
    assert_eq!(new["params"][0]["name"], "$class");
    assert!(new["notes"][0].as_str().unwrap().contains("preamble"), "{new}");
}
