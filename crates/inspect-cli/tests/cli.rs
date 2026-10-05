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

const ISA_SCRIPT: &str = "\
package Base; sub new { bless {}, shift }
package Mid; use parent -norequire, 'Base';
package Other; sub other {}
package Child; use mro 'c3'; use parent -norequire, 'Mid', 'Other';
package Runtime; our @ISA = ('Base');
";

#[test]
fn package_inheritance() {
    let v = run(&["-e", ISA_SCRIPT]);
    let pkgs = &v["packages"];

    // a package with @ISA but no subs is still listed (empty subs)
    let mid = &pkgs["Mid"];
    assert_eq!(mid["subs"], serde_json::json!({}));
    assert_eq!(mid["isa"], serde_json::json!(["Base"]));
    assert_eq!(mid["linear_isa"], serde_json::json!(["Mid", "Base"]));
    assert_eq!(mid["mro"], "dfs");

    // declaration order for isa, C3 order (self first) for linear_isa
    let child = &pkgs["Child"];
    assert_eq!(child["isa"], serde_json::json!(["Mid", "Other"]));
    assert_eq!(
        child["linear_isa"],
        serde_json::json!(["Child", "Mid", "Base", "Other"])
    );
    assert_eq!(child["mro"], "c3");

    // no @ISA -> no inheritance fields
    assert!(pkgs["Base"].get("isa").is_none());
    assert!(pkgs["Base"].get("linear_isa").is_none());

    // a top-level `our @ISA = (...)` of the compiled file is a runtime
    // assignment and is not seen (the main body never runs)
    assert!(pkgs.get("Runtime").is_none());
}

const LEXICALS_SCRIPT: &str = "\
use v5.36;
package Foo; sub new { bless {}, shift }
package Bar;
my $outer = 1;
sub run ($self, $n = 2) {
    my Foo $obj = Foo->new;
    our $pkgvar;
    state $count = 0;
    for my $i (1..$n) {
        $count += $i + $outer;
    }
    return $obj;
}
";

#[test]
fn sub_lexicals() {
    let v = run(&["-e", LEXICALS_SCRIPT]);
    let lex = v["packages"]["Bar"]["subs"]["run"]["lexicals"]
        .as_array()
        .expect("lexicals array");
    let by_name = |name: &str| {
        lex.iter()
            .find(|e| e["name"] == name)
            .unwrap_or_else(|| panic!("{name} not in {lex:?}"))
    };

    // signature parameters: declared on the sub line, visible to the body
    assert_eq!(by_name("$self")["decl"], "my");
    assert_eq!(by_name("$self")["line"], 5);
    assert_eq!(by_name("$n")["scope"], serde_json::json!([5, 12]));

    // `my Foo $obj`: declared type; visible from the next statement on
    let obj = by_name("$obj");
    assert_eq!(obj["type"], "Foo");
    assert_eq!(obj["line"], 6);
    assert_eq!(obj["scope"], serde_json::json!([7, 12]));

    assert_eq!(by_name("$pkgvar")["decl"], "our");
    assert_eq!(by_name("$pkgvar")["package"], "Bar");
    assert_eq!(by_name("$count")["decl"], "state");

    // loop variable: scoped to the loop body
    let i = by_name("$i");
    assert_eq!(i["line"], 9);
    assert_eq!(i["scope"], serde_json::json!([10, 10]));

    // closure capture: marked outer, no lines
    let outer = by_name("$outer");
    assert_eq!(outer["outer"], true);
    assert!(outer.get("line").is_none());

    // sigil-only internal slots are not listed
    assert!(
        lex.iter()
            .all(|e| e["name"].as_str().is_some_and(|n| n.len() > 1))
    );
}

#[test]
fn analyze_prefix() {
    // a throwaway "workspace" with one module under lib/
    let ws = std::env::temp_dir().join(format!("perl-inspect-prefix-{}", std::process::id()));
    let lib = ws.join("lib").join("My");
    std::fs::create_dir_all(&lib).expect("mkdir");
    std::fs::write(
        lib.join("Util.pm"),
        "package My::Util;\nsub add {\n    my ($x, $y) = @_;\n    return $x + $y;\n}\n1;\n",
    )
    .expect("write module");
    let inc = format!("-I{}", ws.join("lib").display());
    let prefix = format!("--analyze-prefix={}", ws.join("lib").display());
    let script = "use My::Util; sub here { 1 }";

    // default: subs of other files are not analyzed
    let v = run(&[&inc, "-e", script]);
    let add = &v["packages"]["My::Util"]["subs"]["add"];
    assert_eq!(add["provenance"], "imported");
    assert!(add.get("args").is_none());
    assert!(add.get("lexicals").is_none());

    // with the prefix: analyzed like a file sub, provenance unchanged
    let v = run(&[&prefix, &inc, "-e", script]);
    let _ = std::fs::remove_dir_all(&ws);
    let add = &v["packages"]["My::Util"]["subs"]["add"];
    assert_eq!(add["provenance"], "imported");
    assert_eq!(add["lines"], serde_json::json!([3, 4]));
    assert_eq!(add["args"]["max_arity"], 2);
    assert_eq!(add["lexicals"][0]["name"], "$x");
    // subs outside the prefix (core modules) stay unanalyzed
    let others_analyzed = v["packages"]
        .as_object()
        .expect("packages")
        .iter()
        .filter(|(pkg, _)| *pkg != "My::Util")
        .flat_map(|(_, p)| p["subs"].as_object().expect("subs").values())
        .any(|s| s["provenance"] == "imported" && s.get("args").is_some());
    assert!(!others_analyzed);
}
