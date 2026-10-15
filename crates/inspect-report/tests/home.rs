//! U7: `home` of a sub named somewhere other than where it is listed.
//!
//! One embedded interpreter per test process, as in api.rs.

use libperl_rs::Perl;
use serde_json::json;

const SCRIPT: &str = "\
package Ho::Lib;
use Exporter 'import';
BEGIN { our @EXPORT_OK = qw(helper) }
sub helper { 1 }
sub orig { 2 }
BEGIN { $INC{'Ho/Lib.pm'} = __FILE__ }
package Ho::User;
use Scalar::Util qw(blessed);
use List::Util qw(first);
BEGIN { Ho::Lib->import('helper') }
sub own { 3 }
BEGIN { *Ho::User::alias = \\&Ho::Lib::orig; *Ho::User::fn = sub { 4 } }
package main;
sub plain { 5 }
BEGIN { *Ho::User::from_main = $main::{plain} }
BEGIN { $main::moose = eval q{ package Ho::Moosy; use Moose; 1 } }
";

#[test]
fn sub_home() {
    let args: Vec<String> = ["inspect-report-test", "-e", SCRIPT]
        .map(String::from)
        .into();
    let envp: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();

    let mut perl = Perl::new();
    inspect_report::prepare(&perl);
    assert_eq!(perl.parse(&args, &envp), 0, "parse failed");
    let v = inspect_report::report(&perl, "-e", &Default::default());
    let user = &v["packages"]["Ho::User"]["subs"];
    let home = |name: &str| user[name]["home"].clone();

    // Exporter import of a Perl sub
    assert_eq!(
        home("helper"),
        json!({ "package": "Ho::Lib", "name": "helper" })
    );
    // XS import: Scalar::Util's blessed is builtin::blessed on newer perls
    let blessed = home("blessed");
    assert_eq!(blessed["name"], "blessed");
    assert!(
        ["Scalar::Util", "builtin"].contains(&blessed["package"].as_str().unwrap_or("")),
        "{blessed}"
    );
    assert_eq!(
        home("first"),
        json!({ "package": "List::Util", "name": "first" })
    );
    // glob alias
    assert_eq!(
        home("alias"),
        json!({ "package": "Ho::Lib", "name": "orig" })
    );
    // anonymous sub assigned to a glob
    assert_eq!(
        home("fn"),
        json!({ "package": "Ho::User", "name": "__ANON__", "anon": true })
    );
    // a CvNAMED sub (main's subs are stored as RV to CV) aliased elsewhere
    assert_eq!(
        home("from_main"),
        json!({ "package": "main", "name": "plain" })
    );

    // defined where listed: no home
    assert_eq!(user["own"].get("home"), None);
    assert_eq!(v["packages"]["Ho::Lib"]["subs"]["helper"].get("home"), None);
    assert_eq!(v["packages"]["main"]["subs"]["plain"].get("home"), None);

    // Moose, when installed: an imported keyword vs the class's own meta
    let moosy = &v["packages"]["Ho::Moosy"]["subs"];
    if moosy.is_object() {
        assert_eq!(
            moosy["has"]["home"],
            json!({ "package": "Moose", "name": "has" })
        );
        assert_eq!(moosy["meta"].get("home"), None, "{}", moosy["meta"]);
    } else {
        eprintln!("Moose not installed: skipped its checks");
    }
}
