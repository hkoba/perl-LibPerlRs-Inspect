//! Debugger-mode compile (`dbsrc`) + sub listing (`subs`) + range logic
//! over a live interpreter. One test function: one embedded interpreter
//! per process.

use inspect_capture::{dbsrc, subs};
use inspect_core::passes::{self, logic};
use libperl_rs::Perl;

const SCRIPT: &str = "\
sub f {
 my ($x) = @_;
 if ($x) {
  note();
  return 1;
 }
 done();
 return 2;
}
BEGIN { eval qq{sub evsub {\\n my (\\$y) = \\@_;\\n \\$y ? 1 : 0\\n}}; die $@ if $@ }
my $anon = sub {
 my ($z) = @_;
 return $z // 0;
};
print f(1);
";

#[test]
fn dbsrc_subs_and_range() {
    let args: Vec<String> = ["inspect-dbsrc-test", "-e", SCRIPT].map(String::from).into();
    let mut envp: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    envp.retain(|e| !e.starts_with("PERL5DB="));
    envp.push(dbsrc::perl5db_env(dbsrc::PERLDB_BITS));

    let mut perl = Perl::new();
    dbsrc::perldb_prepare(&perl, dbsrc::PERLDB_BITS);
    assert_eq!(perl.parse(&args, &envp), 0, "parse failed");
    assert_eq!(dbsrc::perldb(&perl), dbsrc::PERLDB_BITS, "$^P kept");

    // sources kept: the main program and the eval
    let main_src = dbsrc::file_source_lines(&perl, "-e").expect("source of -e");
    assert_eq!(main_src[1].as_deref(), Some("sub f {\n"));
    assert_eq!(main_src[8].as_deref(), Some(" return 2;\n"));
    let files = dbsrc::known_files(&perl);
    assert!(files.iter().any(|f| f == "-e"), "files = {files:?}");
    let eval_file = files
        .iter()
        .find(|f| f.starts_with("(eval "))
        .expect("an eval file is listed");
    let eval_src = dbsrc::file_source_lines(&perl, eval_file).expect("eval source");
    assert_eq!(eval_src[2].as_deref(), Some(" my ($y) = @_;\n"));

    // subs: f in -e, evsub in the eval (NAMEEVAL names its origin)
    let all = subs::list_subs(&perl);
    let f = all.iter().find(|s| s.qualified_name() == "main::f").expect("main::f");
    assert_eq!(f.file.as_deref(), Some("-e"));
    assert_eq!(f.lines, Some((2, 8)));
    assert!(!f.xsub);
    let ev = all.iter().find(|s| s.name == "evsub").expect("main::evsub");
    assert!(
        ev.file.as_deref().is_some_and(|f| f.starts_with("(eval ") && f.contains("[-e:10]")),
        "evsub file = {:?}",
        ev.file
    );
    assert_eq!(ev.lines, Some((2, 3)));
    // the PERL5DB preamble is compiled as part of the main file, so the
    // stub DB::DB is listed under "-e" as well
    let mut in_main: Vec<String> = subs::subs_in_file(&perl, "-e")
        .iter()
        .map(|s| s.qualified_name())
        .collect();
    in_main.sort();
    assert_eq!(in_main, ["DB::DB", "main::f"]);
    assert!(subs::find_cv(&perl, "main::f").is_some());
    assert!(subs::find_cv(&perl, "f").is_some());
    assert!(subs::find_cv(&perl, "main::nope").is_none());

    // dbstate ops are COPs: the logic pass does not notice the mode
    let ir = subs::capture_sub_by_name(&perl, "main::f").expect("capture f");
    assert!(
        ir.nodes().iter().any(|n| n.name == "dbstate"),
        "compiled under PERLDBf_LINE"
    );
    let l = logic::analyze_logic(&ir);
    assert_eq!(l.conds, ["$x"]);
    assert_eq!(l.paths.len(), 2);
    let r = logic::analyze_logic_range(&ir, 4..=5).expect("range inside the arm");
    assert!(r.in_block_of.is_some(), "lines 4-5 lie inside the if arm");
    assert_eq!(r.logic.paths[0].kind, "return");

    // the main program: statements() works on a `leave` root, and the
    // anonymous sub is listed with its CV
    let main = inspect_capture::capture_main_full(&perl).expect("capture main");
    assert!(!passes::statements(&main.ir).is_empty(), "main has statements");
    assert_eq!(main.ir.anon_subs.len(), 1, "anon_subs = {:?}", main.ir.anon_subs);
    assert_eq!(main.ir.anon_subs[0].lines, Some((12, 13)));
    assert_eq!(main.anon_cvs.len(), 1);
    assert_eq!(main.anon_cvs[0].0, main.ir.anon_subs[0].node_id);
    let anon_ir = inspect_capture::capture_sub(&perl, main.anon_cvs[0].1).expect("capture anon");
    assert_eq!(logic::analyze_logic(&anon_ir).conds, ["defined($z)"]);
    let whole = logic::analyze_logic_range(&main.ir, 1..=100).expect("main range");
    assert!(!whole.tail, "a main program has no implicit value");
    assert_eq!(whole.logic.paths.last().map(|p| p.kind.as_str()), Some("fallthrough"));
}
