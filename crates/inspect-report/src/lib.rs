//! inspect-report — the semantic JSON report of LibPerlRs::Inspect, as a
//! library.
//!
//! Given an embedded interpreter that has *compiled* (but not run) a
//! target, [`report`] produces the `schema_version` 1 JSON that the
//! `perl-inspect` CLI prints:
//!   - stash walk + **provenance tags** (file / imported / xs)
//!   - file / line range / prototype per CV
//!   - argspec of file-defined subs (or, with [`ReportOpts::deep`], the
//!     full analysis report: args / returns / logic / types / lints); with
//!     [`ReportOpts::analyze_prefixes`] also of subs from other files under
//!     the given path prefixes (e.g. the workspace's `lib/`)
//!   - the target file's own `use` statements (`uses`) and opaque `BEGIN`
//!     blocks (`opaque_begins`)
//!   - `lexicals` of each analyzed sub: name, `pad_ix`, `decl`
//!     (my / state / our / field), declared `type` (`my Foo $x`), the
//!     `package` of an `our`, the declaring `line` and the `scope` line
//!     range of the statements that can see it (from the pad name's COP
//!     sequence range); closure captures are `outer` without lines
//!   - `file_lexicals`: the same for the top level of the compiled file
//!     (`my $config`, `my Foo $obj`, `our $VERSION`, block and loop
//!     variables). Taken from the main CV when the file is the main
//!     program, otherwise from the pad kept when a `require` compiled it
//!     after [`prepare`] (e.g. `-e 'BEGIN { require $ARGV[0] }' -- FILE`,
//!     or a `require` run after parse). When it cannot be obtained the key
//!     is left out and `file_lexicals_error` says why
//!   - `home` of a sub that was named somewhere other than where it is
//!     listed: `{"package", "name"}` of the glob it was defined through
//!     (`CvGV`; for a `CvNAMED` sub its name and `CvSTASH`), e.g.
//!     `{"package": "Carp", "name": "confess"}` for an imported
//!     `confess`, or the original name for a glob alias. An anonymous
//!     sub assigned to a glob has `"name": "__ANON__", "anon": true`.
//!     Left out when it equals the listed package and name — so subs
//!     defined in their own package, including generated ones such as
//!     accessors, have none — and when it cannot be read. XSUBs too
//!   - per-package inheritance for packages with a non-empty `@ISA`:
//!     `isa` (declaration order), `linear_isa` (MRO order, the package
//!     itself first) and `mro` (`dfs` / `c3`). Such a package is listed
//!     even when it has no subs (`"subs": {}`). `@ISA` is read as it
//!     stands after compilation: assignments in modules loaded via
//!     `use`/`require` (which execute) and in BEGIN (`use parent`) are
//!     seen, but a top-level runtime assignment in the compiled file
//!     itself (`our @ISA = (...)`) is not, since its main body never runs.
//!
//! # Schema compatibility
//!
//! The JSON stays at `schema_version` 1 while it only grows: new keys are
//! optional and existing ones keep their meaning. Detect newer features
//! by the presence of their keys.
//!
//! # Calling sequence
//!
//! The interpreter must be prepared **before** `perl_parse` so that the
//! BEGIN blocks run during compilation are kept for `uses` extraction:
//!
//! ```no_run
//! use libperl_rs::Perl;
//!
//! let args: Vec<String> = ["perl", "-Ilib", "lib/Foo.pm"].map(String::from).into();
//! let envp: Vec<String> = std::env::vars().map(|(k, v)| format!("{k}={v}")).collect();
//!
//! let mut perl = Perl::new();
//! inspect_report::prepare(&perl);          // 1. before parse
//! let rc = perl.parse(&args, &envp);       // 2. compile (BEGIN/use run, main body does not)
//! if rc != 0 {
//!     // compile error: perl has already written the message to stderr
//!     return;
//! }
//! let main_file = inspect_report::main_file(&perl).expect("$0 is set after parse"); // 3.
//! let json = inspect_report::report(&perl, &main_file, &Default::default());        // 4.
//! println!("{json}");
//! ```
//!
//! `Perl::run` is never needed (and should not be called): the trust model
//! is the same as `perl -c`.
//!
//! [`report`] is **one-shot**: it requires `B::Deparse` inside the
//! interpreter, which pollutes the symbol table. Use a throwaway
//! interpreter (e.g. one per forked child) for each report.

use std::collections::BTreeMap;

use inspect_capture::packages;
use libperl_rs::{Perl, StashWalker, SubEntry};
use serde_json::{Map, Value, json};

/// The `schema_version` of the JSON produced by [`report`].
pub const SCHEMA_VERSION: u32 = 1;

/// Options for [`report`].
///
/// Construct with `ReportOpts::default()` and set fields; new fields may
/// be added in the future.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ReportOpts {
    /// Embed the full analysis report (args / returns / logic / types /
    /// lints) in each file-defined sub instead of argspec alone.
    pub deep: bool,
    /// Also analyze (lines / args or report / lexicals) the subs defined
    /// in other files whose path (CvFILE) starts with one of these
    /// prefixes — e.g. the workspace's `lib/`. Plain string prefix match:
    /// pass them in the form perl loads the files, i.e. matching the `-I`
    /// paths. Such subs keep their `imported` provenance. Empty (the
    /// default) analyzes only the compiled file's own subs.
    pub analyze_prefixes: Vec<String>,
}

/// Prepare a fresh interpreter for [`report`]. Must be called **before**
/// `Perl::parse`.
///
/// Enables BEGIN capture (`PL_savebegin`) so that the BEGIN CVs run during
/// parse are saved into `PL_beginav_save`; without it `uses` and
/// `opaque_begins` come out empty. Also starts keeping the top-level pad
/// of each file `require` compiles (process-wide, by wrapping
/// `PL_ppaddr[OP_REQUIRE]`), for `file_lexicals` of a file compiled as a
/// module.
pub fn prepare(perl: &Perl) {
    inspect_capture::enable_begin_capture(perl);
    inspect_capture::filescope::enable();
}

/// The main file as perl sees it (`$0`): the script path, or `"-e"`.
///
/// Call after a successful `Perl::parse`; pass the result to [`report`].
pub fn main_file(perl: &Perl) -> Option<String> {
    let sv0 = perl.get_sv("0", 0)?;
    Some(String::from_utf8_lossy(sv0.pv(perl)).into_owned())
}

/// Build the `schema_version` 1 report of a compiled interpreter.
///
/// `main_file` decides the provenance tag (`"file"` means CvFILE equals
/// it) and which BEGINs count as the file's own `use`s; normally it is
/// [`main_file`]. See the crate docs for the calling sequence and the
/// one-shot restriction.
pub fn report(perl: &Perl, main_file: &str, opts: &ReportOpts) -> Value {
    // package → sub name → entry (BTreeMap makes the output order deterministic)
    let mut pkgs: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    let mut walker = StashWalker::new(perl);
    walker.walk("main", &mut |e| {
        let entry = sub_entry(perl, e, main_file, opts);
        pkgs.entry(e.package.clone())
            .or_default()
            .insert(e.name.clone(), entry);
    });

    // Inheritance (U5). Done before file_begins as well: B::Deparse would
    // add its own packages. A package with @ISA but no subs still gets an
    // entry (with empty "subs").
    let mut inheritance: BTreeMap<String, packages::Inheritance> = BTreeMap::new();
    for pkg in packages::package_names(perl) {
        if let Some(inh) = packages::inheritance(perl, &pkg) {
            inheritance.insert(pkg, inh);
        }
    }
    for pkg in inheritance.keys() {
        pkgs.entry(pkg.clone()).or_default();
    }

    let packages: BTreeMap<String, Value> = pkgs
        .into_iter()
        .map(|(pkg, subs)| {
            let mut entry = json!({ "subs": subs });
            if let Some(inh) = inheritance.remove(&pkg) {
                entry["isa"] = json!(inh.isa);
                entry["linear_isa"] = json!(inh.linear_isa);
                entry["mro"] = json!(inh.mro);
            }
            (pkg, entry)
        })
        .collect();

    // use extraction is done **after** the stash walk: requiring B::Deparse
    // pollutes the symbol table (see the note in inspect-capture begins.rs)
    let (uses, opaque_begins, uses_error) = match inspect_capture::file_begins(perl, main_file) {
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
    match file_lexicals(perl, main_file) {
        Ok(lex) => out["file_lexicals"] = lex,
        Err(e) => out["file_lexicals_error"] = json!(e),
    }
    out
}

/// Top-level lexicals of `main_file` (U6): from the main CV when it is
/// the main program (`$0`), otherwise from the pad kept when `require`
/// compiled it (see [`prepare`]).
fn file_lexicals(perl: &Perl, main_file: &str) -> Result<Value, String> {
    if self::main_file(perl).as_deref() == Some(main_file) {
        return inspect_capture::capture_main(perl).map(|ir| lexicals(&ir.pad));
    }
    inspect_capture::filescope::file_pad(main_file)
        .map(|pad| lexicals(&pad))
        .ok_or_else(|| {
            format!(
                "the top level of {main_file} was not seen compiled: it is not the main program, \
                 and no require compiled it after prepare()"
            )
        })
}

/// Build the report entry for a single sub.
///
/// Provenance tag: XSUB → "xs", CvFILE equal to the target file → "file",
/// anything else (imported from another module / defined in another file)
/// → "imported". "file" subs, and "imported" ones under
/// `opts.analyze_prefixes`, are analyzed (lines / args or report /
/// lexicals).
fn sub_entry(perl: &Perl, e: &SubEntry, main_file: &str, opts: &ReportOpts) -> Value {
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
    // Where the sub was named (U7), when it is not where it is listed:
    // an import, a glob alias, or an anonymous sub assigned to a glob
    if let Some(h) = packages::cv_home(cv)
        && (h.package != e.package || h.name != e.name)
    {
        let mut home = json!({ "package": h.package, "name": h.name });
        if h.anon {
            home["anon"] = json!(true);
        }
        m.insert("home".into(), home);
    }
    // Where the glob was created (a clue for tracking imports). Note that
    // simple subs are stored in the stash as RV→CV and have no glob (gv: None).
    if let Some(gv) = &e.gv {
        if let (Some(gf), Some(gl)) = (gv.file(), gv.line()) {
            m.insert("gv".into(), json!({ "file": gf, "line": gl }));
        }
    }

    let analyze = provenance == "file"
        || (provenance == "imported"
            && file.as_deref().is_some_and(|f| {
                opts.analyze_prefixes
                    .iter()
                    .any(|p| !p.is_empty() && f.starts_with(p.as_str()))
            }));
    if analyze {
        match inspect_capture::capture_sub(perl, cv) {
            Ok(ir) => {
                if let Some((lo, hi)) = ir.lines {
                    m.insert("lines".into(), json!([lo, hi]));
                }
                m.insert("lexicals".into(), lexicals(&ir.pad));
                if opts.deep {
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

/// The named pad entries of a sub (U2): name, pad index, declarator,
/// declared type (`my Foo $x`), declaring line and the line range of the
/// statements that can see it. Variables captured from an enclosing sub
/// are marked `outer` and carry no lines. Sigil-only names (`$`) are
/// internal slots (state-var once flags, loop iterators, signature
/// placeholders) and are left out.
fn lexicals(pad: &[inspect_core::ir::PadEntry]) -> Value {
    use inspect_core::ir::padname_flags;
    let entries: Vec<Value> = pad
        .iter()
        .filter(|p| p.name.as_deref().is_some_and(|n| n.len() > 1))
        .map(|p| {
            let decl = if p.our_stash.is_some() {
                "our"
            } else if p.flags & padname_flags::FIELD != 0 {
                "field"
            } else if p.flags & padname_flags::STATE != 0 {
                "state"
            } else {
                "my"
            };
            let mut e = Map::new();
            e.insert("name".into(), json!(p.name));
            e.insert("pad_ix".into(), json!(p.ix));
            e.insert("decl".into(), json!(decl));
            if let Some(t) = &p.typ {
                e.insert("type".into(), json!(t));
            }
            if let Some(pkg) = &p.our_stash {
                e.insert("package".into(), json!(pkg));
            }
            if p.flags & padname_flags::OUTER != 0 {
                e.insert("outer".into(), json!(true));
            }
            if let Some(l) = p.line {
                e.insert("line".into(), json!(l));
            }
            if let Some((lo, hi)) = p.scope {
                e.insert("scope".into(), json!([lo, hi]));
            }
            Value::Object(e)
        })
        .collect();
    Value::Array(entries)
}
