//! U6: `file_lexicals` of files compiled as modules, i.e. by `require`:
//! from `BEGIN { require FILE }` during perl_parse, and from a `require`
//! run with eval_pv after it (a check in an interpreter that has been
//! parsed already).
//!
//! One embedded interpreter per test process, as in api.rs.

use std::ffi::CString;

use libperl_rs::Perl;
use serde_json::{Value, json};

const BODY: &str = "\
use strict;
our $VERSION = '1.0';
my $config = { a => 1 };
my Lx::Base $obj;
{
    my $inner = 2;
}
sub show { $config }
1;
";

fn expected() -> Value {
    json!([
        { "name": "$VERSION", "decl": "our", "package": "Lx::PKG",
          "line": 3, "scope": [4, 10] },
        { "name": "$config", "decl": "my", "line": 4, "scope": [5, 10] },
        { "name": "$obj", "decl": "my", "type": "Lx::Base",
          "line": 5, "scope": [7, 10] },
        { "name": "$inner", "decl": "my", "line": 7 },
    ])
}

#[test]
fn module_file_lexicals() {
    let ws = std::env::temp_dir().join(format!("inspect-report-modlex-{}", std::process::id()));
    std::fs::create_dir_all(ws.join("Lx")).expect("mkdir");
    std::fs::write(ws.join("Lx/Base.pm"), "package Lx::Base;\n1;\n").expect("write");
    let cold = ws.join("Lx/Cold.pm").display().to_string();
    let warm = ws.join("Lx/Warm.pm").display().to_string();
    std::fs::write(
        &cold,
        format!("package Lx::Cold;\nuse Lx::Base;\n{}", &BODY[12..]),
    )
    .expect("write");
    std::fs::write(
        &warm,
        format!("package Lx::Warm;\nuse Lx::Base;\n{}", &BODY[12..]),
    )
    .expect("write");

    let inc = format!("-I{}", ws.display());
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
    let envp: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();

    let mut perl = Perl::new();
    inspect_report::prepare(&perl);
    assert_eq!(perl.parse(&args, &envp), 0, "parse failed");

    // after parse: require the second file at run time
    let code = CString::new(format!("require '{warm}'; 1")).unwrap();
    unsafe { libperl_rs::thx::Perl_eval_pv(perl.as_ptr(), code.as_ptr(), 1) };

    // report() is one-shot for the symbol table (B::Deparse); a second
    // call is fine for file_lexicals, which do not depend on it
    for (file, pkg) in [(&warm, "Lx::Warm"), (&cold, "Lx::Cold")] {
        let v = inspect_report::report(&perl, file, &Default::default());
        assert!(v.get("file_lexicals_error").is_none(), "{file}: {v}");
        let want = serde_json::to_string(&expected())
            .unwrap()
            .replace("Lx::PKG", pkg);
        assert_eq!(
            without_pad_ix(&v["file_lexicals"]),
            serde_json::from_str::<Value>(&want).unwrap(),
            "{file}"
        );
    }

    // a file nobody compiled
    let v = inspect_report::report(&perl, "/no/such/File.pm", &Default::default());
    assert!(v.get("file_lexicals").is_none());
    assert!(
        v["file_lexicals_error"]
            .as_str()
            .is_some_and(|e| e.contains("not seen"))
    );
    let _ = std::fs::remove_dir_all(&ws);
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
