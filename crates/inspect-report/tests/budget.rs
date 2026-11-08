//! U8: the analyses of a sub stay bounded. Each body below is a shape
//! that once made the symbolic execution or the logic walk run for
//! minutes and take gigabytes (found in Getopt::Long, Pod::Simple and
//! Hash::MoreUtils). With the limits switched off (`value_size` and
//! `work` at their maximum) `shift_chain` takes about 3000 times as long
//! and `self_store` grows without bound; that is not run here, since it
//! can exhaust the memory of the machine.
//!
//! One embedded interpreter per test process, as in api.rs.

use std::time::{Duration, Instant};

use inspect_core::passes::{argspec, logic, symexec::Limits};
use libperl_rs::Perl;

/// Branches on a list that is shifted under them: every `shift` makes
/// `@list` a joined value whose length the next condition tests, and the
/// alternatives of a joined value carry those conditions.
const SHIFT_CHAIN: &str = "
sub shift_chain {
    my ($argv, @list) = @_;
    my ($u, $p, $q, $r, $s);
    if (@list && ref($list[0])) { $u = shift(@list) }
    if (@list && $list[0] =~ /^\\W+$/ && !($list[0] eq '<>' && @list > 1 && ref($list[1]))) { $p = shift(@list) }
    if (@list && ref($list[0]) eq 'HASH') { $q = shift(@list) }
    if (@list && !ref($list[0]) && @list > 1) { $r = shift(@list) }
    if (@list && defined($list[0]) && $list[0] ne '--') { $s = shift(@list) }
    return ($u, $p, $q, $r, $s);
}
";

/// A loop with a known condition that stores a copy of a hash into it.
const SELF_STORE: &str = "
sub self_store {
    my ($code) = @_;
    my %r;
    my $i = 0;
    while ($i < 40) { $r{$i} = [ %r ]; $i++ }
    return %r;
}
";

/// Six guards whose arms fall through (64 routes carried on), then a
/// run of early returns, each reached on every route: more outcomes
/// than the logic result keeps.
fn guard_run() -> String {
    let mut body = String::from("sub guard_run {\n    my ($a, $b, $c, $d, $e, $f, $g) = @_;\n    my $n = 0;\n");
    for (i, v) in ["$a", "$b", "$c", "$d", "$e", "$f"].iter().enumerate() {
        body += &format!("    if ({v} || $g > {i}) {{ $n += {i} }} else {{ $n -= {i} }}\n");
    }
    for k in 0..20 {
        body += &format!("    return {k} if $n == {k} && $g;\n");
    }
    body + "    $n;\n}\n"
}

fn sub_ir(perl: &Perl, name: &str) -> inspect_core::ir::SubIr {
    let cv = inspect_capture::subs::find_cv(perl, name).unwrap_or_else(|| panic!("no sub {name}"));
    inspect_capture::capture_sub(perl, cv).expect("capture")
}

/// Run `f`, returning its result and how long it took.
fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let t = Instant::now();
    let v = f();
    (v, t.elapsed())
}

#[test]
fn analyses_stay_bounded() {
    let code = format!("{SHIFT_CHAIN}{SELF_STORE}{}", guard_run());
    let args: Vec<String> = ["inspect-report-test", "-e", &code].map(String::from).into();
    let envp: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
    let mut perl = Perl::new();
    inspect_report::prepare(&perl);
    assert_eq!(perl.parse(&args, &envp), 0, "parse failed");

    // Generous for a debug build on a slow CI runner; each takes a few
    // milliseconds in a release build.
    let bound = Duration::from_secs(5);
    let src = inspect_core::passes::symexec::NoSource;

    for name in ["main::shift_chain", "main::self_store"] {
        let ir = sub_ir(&perl, name);
        let (spec, took) = timed(|| argspec::analyze_args_with(&ir, &src, &Limits::default()));
        assert!(took < bound, "{name}: argspec took {took:?}");
        assert!(!spec.params.is_empty(), "{name}: {spec:?}");

    }

    let ir = sub_ir(&perl, "main::guard_run");
    let (l, took) = timed(|| logic::analyze_logic(&ir));
    assert!(took < bound, "guard_run: logic took {took:?}");
    assert!(l.truncated, "guard_run: expected a truncated result");
    assert!(!l.paths.is_empty());

    // the whole report, deep included
    let mut opts = inspect_report::ReportOpts::default();
    opts.deep = true;
    let (v, took) = timed(|| inspect_report::report(&perl, "-e", &opts));
    assert!(took < bound * 2, "report took {took:?}");
    let subs = &v["packages"]["main"]["subs"];
    for name in ["shift_chain", "self_store", "guard_run"] {
        assert!(subs[name]["report"]["args"].is_object(), "{name}: {}", subs[name]);
    }
}
