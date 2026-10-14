//! Fixture modules for the file_lexicals tests of module mode.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The top level after the `package` line
const BODY: &str = "\
use Lx::Base;
our $VERSION = '1.0';
my $config = { a => 1 };
my Lx::Base $obj;
{
    my $inner = 2;
}
sub show { $config }
1;
";

/// A temp dir with `Lx/Base.pm`, removed on drop
pub struct Ws(pub PathBuf);

impl Ws {
    pub fn new(tag: &str) -> Ws {
        let dir = std::env::temp_dir().join(format!("inspect-report-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Lx")).expect("mkdir");
        std::fs::write(
            dir.join("Lx/Base.pm"),
            "package Lx::Base;\nmy $base = 1;\n1;\n",
        )
        .expect("write");
        Ws(dir)
    }

    /// Write `Lx/NAME.pm` (package `Lx::NAME`, after the `prelude` lines)
    /// and return its absolute path
    pub fn module(&self, name: &str, prelude: &str) -> String {
        let path = self.0.join(format!("Lx/{name}.pm"));
        std::fs::write(&path, format!("{prelude}package Lx::{name};\n{BODY}")).expect("write");
        path.display().to_string()
    }

    pub fn path(&self, rel: &str) -> String {
        self.0.join(rel).display().to_string()
    }

    pub fn dir(&self) -> &Path {
        &self.0
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `file_lexicals` expected for a module written by [`Ws::module`] in
/// package `pkg`, without `pad_ix`; `shift` is the number of lines the
/// prelude adds
pub fn expected(pkg: &str, shift: u64) -> Value {
    let l = |n: u64| n + shift;
    json!([
        { "name": "$VERSION", "decl": "our", "package": pkg, "line": l(3), "scope": [l(4), l(10)] },
        { "name": "$config", "decl": "my", "line": l(4), "scope": [l(5), l(10)] },
        { "name": "$obj", "decl": "my", "type": "Lx::Base", "line": l(5), "scope": [l(7), l(10)] },
        { "name": "$inner", "decl": "my", "line": l(7) },
    ])
}

/// `lex` without the `pad_ix`es, which must increase (under ithreads
/// constants take pad slots too, so the exact indexes vary by build)
pub fn without_pad_ix(lex: &Value) -> Value {
    let mut prev = 0;
    let out: Vec<Value> = lex
        .as_array()
        .unwrap_or_else(|| panic!("file_lexicals array: {lex}"))
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

pub fn envp() -> Vec<String> {
    std::env::vars().map(|(k, v)| format!("{k}={v}")).collect()
}

/// `require` FILE at run time, after parse (like a check in an
/// interpreter that has been parsed already)
pub fn require_at_runtime(perl: &libperl_rs::Perl, file: &str) {
    let code = std::ffi::CString::new(format!("require '{file}'; 1")).unwrap();
    unsafe { libperl_rs::thx::Perl_eval_pv(perl.as_ptr(), code.as_ptr(), 1) };
}

/// Assert the file_lexicals of `file` in `v`, a report for it
pub fn assert_file_lexicals(v: &Value, file: &str, pkg: &str, shift: u64) {
    assert!(
        v.get("file_lexicals_error").is_none(),
        "{file}: {}",
        v["file_lexicals_error"]
    );
    assert_eq!(
        without_pad_ix(&v["file_lexicals"]),
        expected(pkg, shift),
        "{file}"
    );
}

/// Assert that `v`, a report for a file, has no file_lexicals
pub fn assert_not_captured(v: &Value) {
    assert!(v.get("file_lexicals").is_none(), "{}", v["file_lexicals"]);
    assert!(
        v["file_lexicals_error"]
            .as_str()
            .is_some_and(|e| e.contains("not seen")),
        "{v}"
    );
}
