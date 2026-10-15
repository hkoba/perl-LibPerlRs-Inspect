//! U6: `file_lexicals` of a file compiled as the main program (script
//! mode): the pad of the main CV with the lines of PL_main_root's COPs.
//!
//! One embedded interpreter per test process, as in api.rs.

use libperl_rs::Perl;
use serde_json::{Value, json};

const SCRIPT: &str = "\
package Lx::Thing { sub new { bless {}, shift } }
our $VERSION = '1.0';
my $config = { a => 1 };
my Lx::Thing $obj;
{
    my $inner = 2;
    print $inner;
}
for my $i (1 .. 3) {
    print $i;
}
sub show { print $config }
print $obj;
";

#[test]
fn script_file_lexicals() {
    let args: Vec<String> = ["inspect-report-test", "-e", SCRIPT]
        .map(String::from)
        .into();
    let envp: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();

    let mut perl = Perl::new();
    inspect_report::prepare(&perl);
    assert_eq!(perl.parse(&args, &envp), 0, "parse failed");
    let v = inspect_report::report(&perl, "-e", &Default::default());

    assert!(v.get("file_lexicals_error").is_none(), "{v}");
    assert_eq!(
        without_pad_ix(&v["file_lexicals"]),
        json!([
            { "name": "$VERSION", "decl": "our", "package": "main",
              "line": 2, "scope": [3, 13] },
            { "name": "$config", "decl": "my", "line": 3, "scope": [4, 13] },
            { "name": "$obj", "decl": "my", "type": "Lx::Thing",
              "line": 4, "scope": [6, 13] },
            // a bare block: only its statements see it
            { "name": "$inner", "decl": "my", "line": 6, "scope": [7, 7] },
            // a loop variable: the loop body
            { "name": "$i", "decl": "my", "line": 9, "scope": [10, 10] },
        ])
    );
    // the sub sees $config as a capture, without lines
    assert_eq!(
        v["packages"]["main"]["subs"]["show"]["lexicals"],
        json!([{ "name": "$config", "pad_ix": 1, "decl": "my", "outer": true }])
    );
}

/// `lex` without the `pad_ix`es, which must increase (under ithreads
/// constants take pad slots too, so the exact indexes vary by build)
fn without_pad_ix(lex: &Value) -> Value {
    let mut prev = 0;
    let out: Vec<Value> = lex
        .as_array()
        .expect("file_lexicals array")
        .iter()
        .map(|e| {
            let ix = e["pad_ix"].as_u64().expect("pad_ix");
            assert!(ix > prev, "pad_ix not increasing: {lex}");
            prev = ix;
            let mut e = e.clone();
            e.as_object_mut().unwrap().remove("pad_ix");
            e
        })
        .collect();
    Value::Array(out)
}
