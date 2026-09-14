//! perl-inspect — CLI front end for LibPerlRs::Inspect (MVP).
//!
//! Implements the 5 MVP items of the roadmap
//! (libperl-rs/docs/plan/roadmap-next-projects-2026-08.md §3.2):
//!   1. compile-not-run the target in a throwaway embedded interpreter
//!      (`Perl::parse` only. BEGIN/use do run — the trust model is the same
//!      as `perl -c`. `Perl::run` is never called)
//!   2. stash walk + **provenance tags** (file / imported / xs)
//!   3. file / line range / prototype per CV
//!   4. argspec of file-defined subs (inspect-core's analysis pass)
//!   5. JSON with `schema_version` (stdout)
//!
//! Usage (the remaining arguments are passed to perl_parse as-is):
//!   perl-inspect [--deep] lib/Foo.pm
//!   perl-inspect [--deep] -e 'sub add { my ($x, $y) = @_; $x + $y }'
//!   perl-inspect [--deep] -Ilib script.pl
//!
//! `--deep` embeds the full analysis report
//! (args / returns / logic / types / lints) in each sub instead of argspec.

use std::collections::BTreeMap;
use std::env;
use std::process::ExitCode;

use libperl_rs::{Perl, StashWalker, SubEntry};
use serde_json::{Map, Value, json};

const SCHEMA_VERSION: u32 = 1;

fn usage() -> ExitCode {
    eprintln!(
        "usage: perl-inspect [--deep] <file.pm | -e 'code' | perl args...>\n\
         \n\
         Compiles the target with an embedded perl (BEGIN/use run, the\n\
         main body does not — same trust model as `perl -c`) and prints\n\
         a JSON report of every package/sub visible after compilation."
    );
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut deep = false;
    let mut rest: Vec<String> = Vec::new();
    for a in env::args().skip(1) {
        match a.as_str() {
            "--deep" if rest.is_empty() => deep = true,
            "--help" | "-h" if rest.is_empty() => return usage(),
            _ => rest.push(a),
        }
    }
    if rest.is_empty() {
        return usage();
    }

    // Pass the remaining arguments as perl's argv. The environment is passed
    // through untouched (so that PERL5LIB etc. take effect).
    let mut perl_args = vec!["perl-inspect".to_string()];
    perl_args.extend(rest);
    let envp: Vec<String> = env::vars().map(|(k, v)| format!("{k}={v}")).collect();

    let mut perl = Perl::new();
    // Save BEGINs that run during parse into PL_beginav_save (for use extraction, M2)
    inspect_capture::enable_begin_capture(&perl);
    let rc = perl.parse(&perl_args, &envp);
    if rc != 0 {
        // perl itself has already printed the compile error body to stderr
        eprintln!("perl-inspect: compilation failed (perl_parse rc={rc})");
        return ExitCode::FAILURE;
    }

    let sv0 = perl.get_sv("0", 0).expect("$0 is always set after parse");
    let main_file = String::from_utf8_lossy(sv0.pv(&perl)).into_owned();

    // package → sub name → entry (BTreeMap makes the output order deterministic)
    let mut pkgs: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    let mut walker = StashWalker::new(&perl);
    walker.walk("main", &mut |e| {
        let entry = sub_entry(&perl, e, &main_file, deep);
        pkgs.entry(e.package.clone())
            .or_default()
            .insert(e.name.clone(), entry);
    });

    let packages: BTreeMap<String, Value> = pkgs
        .into_iter()
        .map(|(pkg, subs)| (pkg, json!({ "subs": subs })))
        .collect();

    // use extraction is done **after** the stash walk: requiring B::Deparse
    // pollutes the symbol table (see the note in inspect-capture begins.rs)
    let (uses, opaque_begins, uses_error) =
        match inspect_capture::file_begins(&perl, &main_file) {
            Ok(fb) => {
                let uses: Vec<Value> = fb
                    .uses
                    .iter()
                    .map(|u| json!({ "line": u.line, "stmt": u.stmt }))
                    .collect();
                let opaque: Vec<Value> = fb
                    .opaque_begins
                    .iter()
                    .map(|l| json!({ "line": l }))
                    .collect();
                (uses, opaque, None)
            }
            Err(e) => (Vec::new(), Vec::new(), Some(e)),
        };

    let mut out = json!({
        "schema_version": SCHEMA_VERSION,
        "generator": {
            "name": "perl-inspect (LibPerlRs::Inspect)",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "perl": {
            "version": libperl_rs::PERL_VERSION,
            "threaded": libperl_rs::PERL_THREADED == "threaded",
        },
        "file": main_file,
        "uses": uses,
        "opaque_begins": opaque_begins,
        "packages": packages,
    });
    if let Some(e) = uses_error {
        out["uses_error"] = json!(e);
    }
    println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
    ExitCode::SUCCESS
}

/// Build the report entry for a single sub.
///
/// Provenance tag: XSUB → "xs", CvFILE equal to the target file → "file",
/// anything else (imported from another module / defined in another file)
/// → "imported".
fn sub_entry(perl: &Perl, e: &SubEntry, main_file: &str, deep: bool) -> Value {
    let cv = e.cv;
    let file = cv.file();
    let provenance = if cv.is_xsub() {
        "xs"
    } else if file.as_deref() == Some(main_file) {
        "file"
    } else {
        "imported"
    };

    let mut m = Map::new();
    m.insert("provenance".into(), json!(provenance));
    if let Some(f) = &file {
        m.insert("file".into(), json!(f));
    }
    if let Some(p) = cv.proto() {
        m.insert("prototype".into(), json!(p));
    }
    // Where the glob was created (a clue for tracking imports). Note that
    // simple subs are stored in the stash as RV→CV and have no glob (gv: None).
    if let Some(gv) = &e.gv {
        if let (Some(gf), Some(gl)) = (gv.file(), gv.line()) {
            m.insert("gv".into(), json!({ "file": gf, "line": gl }));
        }
    }

    if provenance == "file" {
        match inspect_capture::capture_sub(perl, cv) {
            Ok(ir) => {
                if let Some((lo, hi)) = ir.lines {
                    m.insert("lines".into(), json!([lo, hi]));
                }
                if deep {
                    let report = inspect_core::passes::analyze(&ir);
                    m.insert(
                        "report".into(),
                        serde_json::to_value(report).expect("serialize report"),
                    );
                } else {
                    let args = inspect_core::passes::argspec::analyze_args(&ir);
                    m.insert(
                        "args".into(),
                        serde_json::to_value(args).expect("serialize argspec"),
                    );
                }
            }
            Err(err) => {
                m.insert("error".into(), json!(err));
            }
        }
    }
    Value::Object(m)
}
