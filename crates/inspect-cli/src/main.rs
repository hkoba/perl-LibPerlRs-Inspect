//! perl-inspect — CLI front end for LibPerlRs::Inspect (MVP).
//!
//! A thin wrapper around the `inspect-report` crate, which builds the report.
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

use std::env;
use std::process::ExitCode;

use inspect_report::ReportOpts;
use libperl_rs::Perl;

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
    // Must precede parse: keeps the BEGINs run during parse for `uses`
    inspect_report::prepare(&perl);
    let rc = perl.parse(&perl_args, &envp);
    if rc != 0 {
        // perl itself has already printed the compile error body to stderr
        eprintln!("perl-inspect: compilation failed (perl_parse rc={rc})");
        return ExitCode::FAILURE;
    }

    let main_file = inspect_report::main_file(&perl).expect("$0 is always set after parse");
    let mut opts = ReportOpts::default();
    opts.deep = deep;
    let out = inspect_report::report(&perl, &main_file, &opts);
    println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
    ExitCode::SUCCESS
}
