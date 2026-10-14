//! Library-level test of the calling sequence documented in the crate:
//! Perl::new → prepare → parse → main_file → report.
//!
//! Kept to a single test function so that only one embedded interpreter
//! exists in this test process.

use libperl_rs::Perl;
use serde_json::json;

const SCRIPT: &str = "\
package Foo;
use File::Basename qw(basename);
BEGIN { our $ready = 1 }
sub add { my ($x, $y) = @_; return $x + $y }
sub hi { basename($0) }
package Kid; use parent -norequire, 'Foo';
use Api::Helper;
";

/// Module for the `analyze_prefixes` check, written under a temp dir
const HELPER_PM: &str = "package Api::Helper;\nsub twice { my ($n) = @_; $n * 2 }\n1;\n";

#[test]
fn prepare_parse_report() {
    let ws = std::env::temp_dir().join(format!("inspect-report-api-{}", std::process::id()));
    std::fs::create_dir_all(ws.join("Api")).expect("mkdir");
    std::fs::write(ws.join("Api").join("Helper.pm"), HELPER_PM).expect("write module");
    let inc = format!("-I{}", ws.display());
    let args: Vec<String> = ["inspect-report-test", &inc, "-e", SCRIPT]
        .map(String::from)
        .into();
    let envp: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();

    let mut perl = Perl::new();
    inspect_report::prepare(&perl);
    assert_eq!(perl.parse(&args, &envp), 0, "parse failed");

    let main_file = inspect_report::main_file(&perl).expect("$0 after parse");
    assert_eq!(main_file, "-e");

    let mut opts = inspect_report::ReportOpts::default();
    opts.deep = true;
    opts.analyze_prefixes = vec![ws.display().to_string()];
    let v = inspect_report::report(&perl, &main_file, &opts);
    let _ = std::fs::remove_dir_all(&ws);

    assert_eq!(v["schema_version"], inspect_report::SCHEMA_VERSION);
    assert_eq!(v["file"], "-e");
    assert_eq!(v["perl"]["threaded"], cfg!(perl_useithreads));

    let add = &v["packages"]["Foo"]["subs"]["add"];
    assert_eq!(add["provenance"], "file");
    assert_eq!(add["lines"], json!([4, 4]));
    assert_eq!(add["report"]["args"]["style"], "unpack");
    // imported from File::Basename
    assert_eq!(
        v["packages"]["Foo"]["subs"]["basename"]["provenance"],
        "imported"
    );

    // prepare() took effect: BEGIN capture feeds uses / opaque_begins
    let uses = v["uses"].as_array().expect("uses array");
    assert!(
        uses.iter().any(|u| u["line"] == 2
            && u["stmt"]
                .as_str()
                .is_some_and(|s| s.contains("File::Basename"))),
        "uses = {uses:?}"
    );
    assert_eq!(v["opaque_begins"], json!([{ "line": 3 }]));
    assert!(v.get("uses_error").is_none());

    // U2: lexicals of an analyzed sub
    assert_eq!(
        add["lexicals"],
        json!([
            { "name": "$x", "pad_ix": 1, "decl": "my", "line": 4, "scope": [4, 4] },
            { "name": "$y", "pad_ix": 2, "decl": "my", "line": 4, "scope": [4, 4] },
        ])
    );

    // U5: a sub-less package with @ISA is listed
    let kid = &v["packages"]["Kid"];
    assert_eq!(kid["subs"], json!({}));
    assert_eq!(kid["isa"], json!(["Foo"]));
    assert_eq!(kid["linear_isa"], json!(["Kid", "Foo"]));
    assert_eq!(kid["mro"], "dfs");

    // U3: a sub from a file under analyze_prefixes is analyzed
    let twice = &v["packages"]["Api::Helper"]["subs"]["twice"];
    assert_eq!(twice["provenance"], "imported");
    assert_eq!(twice["report"]["args"]["max_arity"], 1);
    assert_eq!(twice["lexicals"][0]["name"], "$n");
}
