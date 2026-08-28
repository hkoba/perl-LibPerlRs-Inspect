//! perl-inspect — LibPerlRs::Inspect の CLI フロントエンド (MVP)。
//!
//! roadmap (libperl-rs/docs/plan/roadmap-next-projects-2026-08.md §3.2)
//! の MVP 5 項目を実装する:
//!   1. 対象を使い捨て埋め込みインタプリタで compile-not-run
//!      (`Perl::parse` のみ。BEGIN/use は走る — 信頼モデルは `perl -c` と
//!      同一。`Perl::run` は呼ばない)
//!   2. stash walk + **由来タグ** (file / imported / xs)
//!   3. CV ごとの file / 行範囲 / prototype
//!   4. file 定義 sub の argspec (inspect-core の解析パス)
//!   5. `schema_version` 付き JSON (stdout)
//!
//! 使い方 (残余引数はそのまま perl_parse に渡す):
//!   perl-inspect [--deep] lib/Foo.pm
//!   perl-inspect [--deep] -e 'sub add { my ($x, $y) = @_; $x + $y }'
//!   perl-inspect [--deep] -Ilib script.pl
//!
//! `--deep` は argspec に代えて全解析レポート
//! (args / returns / logic / types / lints) を各 sub に埋め込む。

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

    // 残余引数を perl の argv として渡す。環境は素通し (PERL5LIB 等が
    // 効くように)。
    let mut perl_args = vec!["perl-inspect".to_string()];
    perl_args.extend(rest);
    let envp: Vec<String> = env::vars().map(|(k, v)| format!("{k}={v}")).collect();

    let mut perl = Perl::new();
    // parse 中の BEGIN を PL_beginav_save に退避させる (use 抽出用、M2)
    inspect_capture::enable_begin_capture(&perl);
    let rc = perl.parse(&perl_args, &envp);
    if rc != 0 {
        // compile エラー本文は perl 自身が stderr へ出している
        eprintln!("perl-inspect: compilation failed (perl_parse rc={rc})");
        return ExitCode::FAILURE;
    }

    let sv0 = perl.get_sv("0", 0).expect("$0 is always set after parse");
    let main_file = String::from_utf8_lossy(sv0.pv(&perl)).into_owned();

    // package → sub 名 → entry (BTreeMap で出力順を決定的に)
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

    // use 抽出は stash walk の**後**に行う: B::Deparse の require が
    // symbol table を汚染するため (inspect-capture begins.rs の注意書き)
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

/// 1 つの sub のレポートエントリを構築する。
///
/// 由来タグ: XSUB → "xs"、CvFILE が対象ファイル → "file"、それ以外
/// (他モジュールから import されたもの・別ファイル定義) → "imported"。
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
    // glob の作成位置 (import 追跡の手がかり)。単純 sub は RV→CV 形で
    // stash に置かれ glob を持たない (gv: None) ことに注意。
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
