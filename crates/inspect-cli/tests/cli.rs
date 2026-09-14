//! End-to-end tests for the perl-inspect CLI: spawn the binary and verify
//! the JSON (CARGO_BIN_EXE_* is the path cargo provides to integration
//! tests of bin crates).

use std::process::Command;

use serde_json::Value;

const SCRIPT: &str = "\
package Foo;
sub add { my ($x, $y) = @_; return $x + $y }
sub id { $_[0] }
package Foo::Bar;
sub nested { 42 }
package main;
sub hello { my ($who) = @_; \"hi $who\" }
";

fn run(args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_perl-inspect"))
        .args(args)
        .output()
        .expect("spawn perl-inspect");
    assert!(
        out.status.success(),
        "perl-inspect failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is valid JSON")
}

#[test]
fn mvp_json_report() {
    let v = run(&["-e", SCRIPT]);

    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["file"], "-e");
    assert_eq!(v["perl"]["threaded"], cfg!(perl_useithreads));

    // file-defined sub: provenance tag + line range + argspec
    let add = &v["packages"]["Foo"]["subs"]["add"];
    assert_eq!(add["provenance"], "file");
    assert_eq!(add["file"], "-e");
    assert_eq!(add["lines"], serde_json::json!([2, 2]));
    assert_eq!(add["args"]["style"], "unpack");
    assert_eq!(add["args"]["max_arity"], 2);
    let params: Vec<&str> = add["args"]["params"]
        .as_array()
        .expect("params array")
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert_eq!(params, ["$x", "$y"]);

    // nested packages are walked too
    assert_eq!(
        v["packages"]["Foo::Bar"]["subs"]["nested"]["provenance"],
        "file"
    );
    assert_eq!(v["packages"]["main"]["subs"]["hello"]["provenance"], "file");

    // Even a bare -e compile environment should show XS subs (e.g.
    // mro::method_changed_in) and subs from other files — confirms the
    // provenance tags are effective
    let all_provenances: Vec<String> = v["packages"]
        .as_object()
        .expect("packages object")
        .values()
        .flat_map(|p| p["subs"].as_object().expect("subs object").values())
        .filter_map(|s| s["provenance"].as_str().map(String::from))
        .collect();
    assert!(all_provenances.iter().any(|p| p == "xs"), "no xs entry seen");
}

const USES_SCRIPT: &str = "\
package Demo;
use strict;
use File::Basename qw(basename);
use constant ANSWER => 42;
BEGIN { our $ready = 1 }
sub hi { basename($0) }
";

#[test]
fn uses_extraction() {
    let v = run(&["-e", USES_SCRIPT]);
    // use statements are recovered via BEGIN attribution + begin_is_use reverse conversion (with import arguments)
    let uses = v["uses"].as_array().expect("uses array");
    assert!(
        uses.iter().any(|u| {
            u["line"] == 3
                && u["stmt"].as_str().is_some_and(|s| {
                    s.contains("File::Basename") && s.contains("basename")
                })
        }),
        "uses = {uses:?}"
    );
    // Raw BEGINs not of use form appear in opaque_begins by line number.
    // line-0 injections from sitecustomize / -M are excluded, so for this
    // script there is exactly one entry: the BEGIN at line 5
    assert_eq!(
        v["opaque_begins"],
        serde_json::json!([{ "line": 5 }]),
        "opaque_begins mismatch"
    );
    // no line-0 contamination (on the uses side either)
    assert!(
        !uses.iter().any(|u| u["line"] == 0),
        "line-0 injected use leaked: {uses:?}"
    );
}

#[test]
fn deep_report() {
    let v = run(&["--deep", "-e", SCRIPT]);
    let add = &v["packages"]["Foo"]["subs"]["add"];
    // with --deep, the full analysis report instead of argspec alone
    assert!(add.get("args").is_none());
    assert_eq!(add["report"]["args"]["style"], "unpack");
    assert!(add["report"].get("returns").is_some());
    assert!(add["report"].get("logic").is_some());
}
