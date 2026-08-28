//! perl-inspect CLI の end-to-end テスト: バイナリを spawn して JSON を
//! 検証する (CARGO_BIN_EXE_* は cargo が bin クレートの統合テストに
//! 提供するパス)。

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

    // file 定義 sub: 由来タグ + 行範囲 + argspec
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

    // ネストしたパッケージも歩けている
    assert_eq!(
        v["packages"]["Foo::Bar"]["subs"]["nested"]["provenance"],
        "file"
    );
    assert_eq!(v["packages"]["main"]["subs"]["hello"]["provenance"], "file");

    // 素の -e の compile 環境にも XS (例: mro::method_changed_in) と
    // 他ファイル由来の sub が見えるはず — 由来タグの実効性の確認
    let all_provenances: Vec<String> = v["packages"]
        .as_object()
        .expect("packages object")
        .values()
        .flat_map(|p| p["subs"].as_object().expect("subs object").values())
        .filter_map(|s| s["provenance"].as_str().map(String::from))
        .collect();
    assert!(all_provenances.iter().any(|p| p == "xs"), "no xs entry seen");
}

#[test]
fn deep_report() {
    let v = run(&["--deep", "-e", SCRIPT]);
    let add = &v["packages"]["Foo"]["subs"]["add"];
    // --deep では argspec 単体でなく全解析レポート
    assert!(add.get("args").is_none());
    assert_eq!(add["report"]["args"]["style"], "unpack");
    assert!(add["report"].get("returns").is_some());
    assert!(add["report"].get("logic").is_some());
}
