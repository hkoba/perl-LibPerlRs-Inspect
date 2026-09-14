//! `use` extraction by BEGIN attribution — the **file-compile version** of
//! the technique in perl-LibPerlRs-PartialEval compile.rs (diff window +
//! exact CvFILE match + B::Deparse::begin_is_use reverse conversion).
//! M2 of the roadmap
//! (libperl-rs/docs/plan/roadmap-next-projects-2026-08.md §3.2).
//!
//! Steps:
//!   1. **Before** `perl_parse`, call [`enable_begin_capture`]
//!      (`PL_savebegin = TRUE`) — BEGIN CVs that run during parse are
//!      then saved into `PL_beginav_save`
//!   2. After parse, [`file_begins`]: pick only the BEGINs whose CvFILE
//!      **exactly matches** the target file (excluding BEGINs of other
//!      modules loaded transitively), and reverse-convert them into
//!      `use Foo (...);` statements via `B::Deparse::begin_is_use` inside
//!      the embedded interpreter (undocumented but stable for years — the
//!      same judgement PartialEval makes)
//!
//! Unlike PartialEval's `Handle`, no refcount retain/release is done: the
//! interpreter is assumed to be throwaway, and the CVs are kept alive by
//! `PL_beginav_save` itself. The known limitations are also the same as
//! PartialEval — `#line` changes CvFILE, so such BEGINs escape attribution.
//! Hint-bit pragmas (strict / warnings / feature etc.), for which
//! `begin_is_use` returns `""`, are not listed in uses (they are baked into
//! the COP hints). BEGINs at line 0 (sitecustomize / `-Mmodule` etc.,
//! injected before line 1 of the source) do not come from the source and
//! are excluded.
//!
//! Note: [`file_begins`] requires B / B::Deparse into the interpreter and
//! therefore pollutes the symbol table. Finish the stash walk before
//! calling it.

use libperl_rs::{Cv, Perl, perl_call};
use libperl_sys as sys;

/// One `use`/`no` statement that could be reverse-converted.
pub struct FileUse {
    /// Line number of the statement (first COP of the BEGIN body).
    pub line: Option<u32>,
    /// Statement of the form `use Foo ('a', 'b');` (trailing newline stripped).
    pub stmt: String,
}

/// Result of [`file_begins`]: the `use` statements, plus the line numbers
/// of raw BEGIN blocks that are not of `use` form.
pub struct FileBegins {
    pub uses: Vec<FileUse>,
    pub opaque_begins: Vec<Option<u32>>,
}

/// Set `PL_savebegin`. Must be called **before `perl_parse`**.
pub fn enable_begin_capture(perl: &Perl) {
    let my_perl = perl.as_ptr();
    #[cfg(perl_useithreads)]
    unsafe {
        (*my_perl).Isavebegin = true;
    }
    #[cfg(not(perl_useithreads))]
    unsafe {
        let _ = my_perl;
        sys::PL_savebegin = true;
    }
}

/// From a parsed interpreter, return the BEGINs attributed to `file`
/// (usually `$0`), classified into `use` statements / opaque.
pub fn file_begins(perl: &Perl, file: &str) -> Result<FileBegins, String> {
    let my_perl = perl.as_ptr();

    // 1. Pick the BEGINs from PL_beginav_save whose CvFILE matches exactly
    let mut cvs: Vec<Cv> = Vec::new();
    unsafe {
        let av = sys::PL_beginav_save!(my_perl);
        if !av.is_null() {
            // Internal AV (no magic) — read AvFILLp+1 elements directly
            let n = sys::AvFILLp(av) + 1;
            for i in 0..n {
                let elem = *sys::AvARRAY(av).add(i as usize);
                if elem.is_null() {
                    continue;
                }
                let Some(cv) = Cv::from_raw(elem as *mut sys::CV) else {
                    continue;
                };
                if cv.file().as_deref() == Some(file) {
                    cvs.push(cv);
                }
            }
        }
    }

    // Line number (first COP). A BEGIN at line 0 was "injected by the
    // interpreter / launcher before line 1 of the source", so exclude it —
    // real examples: the `BEGIN { do ".../sitecustomize.pl" if -f ... }`
    // that USE_SITECUSTOMIZE builds (Fedora etc.) insert into every
    // program, and uses coming from `-Mmodule` (in both cases CvFILE is
    // the main file name and CopLINE is 0).
    let mut lines: Vec<Option<u32>> = Vec::with_capacity(cvs.len());
    let mut kept: Vec<Cv> = Vec::with_capacity(cvs.len());
    for cv in cvs {
        let line = cv.first_cop(perl).map(|c| c.line());
        if line == Some(0) {
            continue;
        }
        lines.push(line);
        kept.push(cv);
    }
    let cvs = kept;
    let stmts = begin_stmts(perl, &cvs)?;

    // 2. Classify by the three-valued result of begin_is_use (statement / "" / undef)
    let mut uses = Vec::new();
    let mut opaque_begins = Vec::new();
    for (i, s) in stmts.into_iter().enumerate() {
        match s {
            Some(stmt) if !stmt.is_empty() => uses.push(FileUse {
                line: lines[i],
                stmt: stmt.trim_end_matches('\n').to_string(),
            }),
            Some(_) => {} // hint-bit pragma: carried by COP hints, so not listed
            None => opaque_begins.push(lines[i]),
        }
    }
    Ok(FileBegins { uses, opaque_begins })
}

/// Run the BEGIN CVs through `begin_is_use` inside the embedded interpreter
/// and return, per CV, Some(statement) (Some("") for hint pragmas) / None (opaque).
fn begin_stmts(perl: &Perl, cvs: &[Cv]) -> Result<Vec<Option<String>>, String> {
    if cvs.is_empty() {
        return Ok(Vec::new());
    }
    let my_perl = perl.as_ptr();
    unsafe {
        // Push coderefs to the target CVs onto a scratch package array
        let av = perl_call!(
            my_perl,
            Perl_get_av(
                c"LibPerlRs::Inspect::_cli::begins".as_ptr(),
                sys::GV_ADD as i32,
            )
        );
        for cv in cvs {
            // newRV is the flavor that increments the referent. Ownership of
            // the returned RV (refcnt 1) is handed straight to av_push
            let rv = perl_call!(my_perl, Perl_newRV(cv.as_ptr() as *mut sys::SV));
            perl_call!(my_perl, Perl_av_push(av, rv));
        }

        // The reverse conversion is done on the Perl side. Stringify into
        // @stmts with undef as "U" and everything else prefixed "S"
        // (preserving the distinction between undef and "")
        let code = c"{
            package LibPerlRs::Inspect::_cli;
            our (@begins, @stmts);
            require B; require B::Deparse;
            my $d = B::Deparse->new;
            @stmts = map {
                my $s = eval { $d->begin_is_use(B::svref_2object($_)) };
                defined($s) ? \"S$s\" : \"U\";
            } @begins;
            1;
        }";
        perl_call!(my_perl, Perl_eval_pv(code.as_ptr(), 0));
        if let Some(err) = errsv(perl) {
            return Err(format!("begin_is_use failed: {err}"));
        }

        let sav = perl_call!(
            my_perl,
            Perl_get_av(
                c"LibPerlRs::Inspect::_cli::stmts".as_ptr(),
                sys::GV_ADD as i32,
            )
        );
        let n = sys::AvFILLp(sav) + 1;
        if n as usize != cvs.len() {
            return Err(format!(
                "begin_is_use produced {n} results for {} BEGINs",
                cvs.len()
            ));
        }
        let mut out = Vec::with_capacity(cvs.len());
        for i in 0..n {
            let elem = *sys::AvARRAY(sav).add(i as usize);
            let s = if elem.is_null() {
                String::new()
            } else {
                let sv = libperl_rs::Sv::from_raw_unchecked(elem);
                String::from_utf8_lossy(sv.pv(perl)).into_owned()
            };
            out.push(match s.strip_prefix('S') {
                Some(stmt) => Some(stmt.to_string()),
                None => None, // "U" = undef = BEGIN not of use form
            });
        }
        Ok(out)
    }
}

/// Some(message) if $@ is non-empty.
fn errsv(perl: &Perl) -> Option<String> {
    let msg_sv = perl.get_sv("@", 0)?;
    let msg = msg_sv.pv(perl);
    if msg.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(msg).into_owned())
    }
}
