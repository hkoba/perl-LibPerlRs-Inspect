//! Debugger-mode compilation: keep the source of every compiled file
//! (string evals included) inside the interpreter.
//!
//! With `PL_perldb` set before `perl_parse`, perl compiles `nextstate`
//! ops as `dbstate` (same class, same tree shape otherwise) and, with
//! `PERLDBf_SAVESRC`, stores the lines of every file it compiles in
//! `@{"_<FILE"}`. `PERLDBf_NAMEEVAL` / `NAMEANON` make `CvFILE` of
//! eval'd code `(eval N)[file:line]` and anonymous subs
//! `__ANON__[file:line]`, so their provenance is readable.
//!
//! `Perl_init_debugger` (which creates `$DB::single` and friends) is not
//! exported, so it has to run through the `$^P` magic: pass
//! [`perl5db_env`] in the environment of `perl_parse`. perl then injects
//! that code instead of `perl5db.pl` as the debugger preamble. It also
//! defines an empty `DB::DB`, so code that sets `$DB::single` while
//! compiling cannot die with "No DB::DB routine defined".

use libperl_rs::{Perl, perl_call};
use libperl_sys as sys;

/// LINE | NAMEEVAL | NAMEANON | SAVESRC | SAVESRC_NOSUBS: `dbstate` ops
/// (needed for breakpoints) and every source kept, eval'd code and
/// anonymous subs named by their origin.
pub const PERLDB_BITS: u32 = sys::PERLDBf_LINE
    | sys::PERLDBf_NAMEEVAL
    | sys::PERLDBf_NAMEANON
    | sys::PERLDBf_SAVESRC
    | sys::PERLDBf_SAVESRC_NOSUBS;

/// Set `PL_perldb` (what `$^P` reads). Must be called **before
/// `perl_parse`** to affect the main program.
pub fn perldb_prepare(perl: &Perl, bits: u32) {
    let my_perl = perl.as_ptr();
    #[cfg(perl_useithreads)]
    unsafe {
        (*my_perl).Iperldb = bits;
    }
    #[cfg(not(perl_useithreads))]
    unsafe {
        let _ = my_perl;
        sys::PL_perldb = bits;
    }
}

/// Current `PL_perldb`.
pub fn perldb(perl: &Perl) -> u32 {
    sys::PL_perldb!(perl.as_ptr())
}

/// The `PERL5DB=...` environment entry to pass to `perl_parse` together
/// with [`perldb_prepare`]: re-asserts `bits` through `$^P` (which
/// initializes the debugger variables) and defines an empty `DB::DB`.
pub fn perl5db_env(bits: u32) -> String {
    format!("PERL5DB=BEGIN {{ $^P = {bits} }} sub DB::DB {{}}")
}

/// The source lines of `file` kept by `PERLDBf_SAVESRC`, indexed by line
/// number (index 0 is unused and `None`). `None` when the interpreter has
/// no `@{"_<FILE"}` for it.
pub fn file_source_lines(perl: &Perl, file: &str) -> Option<Vec<Option<String>>> {
    let av = file_source_av(perl, file)?;
    let n = av.len(perl);
    if n == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(match av.get(perl, i) {
            Some(sv) if i > 0 && unsafe { sys::SvOK(sv.as_ptr()) } != 0 => {
                Some(String::from_utf8_lossy(sv.pv(perl)).into_owned())
            }
            _ => None,
        });
    }
    Some(out)
}

/// Whether the interpreter holds source lines for `file`.
pub fn has_source(perl: &Perl, file: &str) -> bool {
    file_source_av(perl, file).is_some_and(|av| av.len(perl) > 1)
}

fn file_source_av(perl: &Perl, file: &str) -> Option<libperl_rs::Av> {
    let name = std::ffi::CString::new(format!("main::_<{file}")).ok()?;
    let p = perl_call!(perl.as_ptr(), Perl_get_av(name.as_ptr(), 0));
    if p.is_null() {
        return None;
    }
    Some(unsafe { libperl_rs::Av::from_raw_unchecked(p) })
}

/// The files the interpreter has compiled: every `_<FILE` glob in `main::`
/// whose source array is non-empty (so XS stubs like `_<perlmain.c` are
/// left out), in symbol-table order.
pub fn known_files(perl: &Perl) -> Vec<String> {
    let Some(stash) = perl.gv_stashpv("main", 0) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, _val) in stash.iter(perl) {
        if let Some(file) = key.strip_prefix(b"_<") {
            let file = String::from_utf8_lossy(file).into_owned();
            if has_source(perl, &file) {
                out.push(file);
            }
        }
    }
    out
}
