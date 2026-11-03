//! Package enumeration and inheritance (`@ISA` / MRO).
//!
//! `StashWalker` only reports subs, so a package that merely sets up
//! inheritance (`package Child; use parent -norequire, 'Base';`) would be
//! invisible. This module walks the stash tree on its own to list every
//! package, then reads `@ISA` and the MRO of each.

use std::collections::HashSet;
use std::ffi::{CStr, CString};

use libperl_rs::{Av, Perl, SvKind, thx};

/// Inheritance of one package. Only produced for a non-empty `@ISA`.
pub struct Inheritance {
    /// `@ISA` in declaration order
    pub isa: Vec<String>,
    /// MRO resolution order, the package itself first
    /// (`mro_get_linear_isa`)
    pub linear_isa: Vec<String>,
    /// MRO algorithm name: "dfs" or "c3"
    pub mro: String,
}

/// Every package reachable from `main::`, in walk order. Same traversal
/// as `StashWalker` (recurse into `Foo::`-suffixed globs, visit each
/// package once) but emitting packages instead of subs.
pub fn package_names(perl: &Perl) -> Vec<String> {
    fn rec(perl: &Perl, pack: &str, seen: &mut HashSet<String>, out: &mut Vec<String>) {
        if !seen.insert(pack.to_string()) {
            return;
        }
        let Some(stash) = perl.gv_stashpv(pack, 0) else {
            return;
        };
        out.push(pack.to_string());
        // Collect children first: recursing while the parent stash is
        // being iterated would reset its iterator (hv_iterinit).
        let children: Vec<String> = stash
            .iter(perl)
            .filter_map(|(key, val)| {
                let SvKind::Glob(_) = val.kind() else {
                    return None;
                };
                let name = String::from_utf8_lossy(key);
                let base = name.strip_suffix("::").filter(|b| !b.is_empty())?;
                Some(if pack == "main" {
                    base.to_string()
                } else {
                    format!("{pack}::{base}")
                })
            })
            .collect();
        for child in children {
            rec(perl, &child, seen, out);
        }
    }
    let mut out = Vec::new();
    rec(perl, "main", &mut HashSet::new(), &mut out);
    out
}

/// `@ISA` / linear ISA / MRO name of `pack`, or `None` when its `@ISA` is
/// absent or empty. Reading does not create `@ISA` (get_av flags = 0).
pub fn inheritance(perl: &Perl, pack: &str) -> Option<Inheritance> {
    let my_perl = perl.as_ptr();
    let isa_name = CString::new(format!("{pack}::ISA")).ok()?;
    let isa_av = unsafe { thx::Perl_get_av(my_perl, isa_name.as_ptr(), 0) };
    if isa_av.is_null() {
        return None;
    }
    let isa = av_strings(perl, isa_av);
    if isa.is_empty() {
        return None;
    }
    let stash = perl.gv_stashpv(pack, 0)?;
    let linear_isa = av_strings(perl, unsafe {
        thx::Perl_mro_get_linear_isa(my_perl, stash.as_ptr())
    });
    let mro = unsafe {
        let meta = thx::HvMROMETA(my_perl, stash.as_ptr());
        let alg = if meta.is_null() {
            std::ptr::null()
        } else {
            (*meta).mro_which
        };
        if alg.is_null() || (*alg).name.is_null() {
            "dfs".to_string()
        } else {
            CStr::from_ptr((*alg).name).to_string_lossy().into_owned()
        }
    };
    Some(Inheritance {
        isa,
        linear_isa,
        mro,
    })
}

/// String values of an AV's elements (undef / missing slots skipped).
fn av_strings(perl: &Perl, av: *mut libperl_rs::AV) -> Vec<String> {
    if av.is_null() {
        return Vec::new();
    }
    let av = unsafe { Av::from_raw_unchecked(av) };
    av.iter(perl)
        .flatten()
        .map(|sv| String::from_utf8_lossy(sv.pv(perl)).into_owned())
        .collect()
}

/// Where a sub was named: the package and name of the glob it was
/// defined through (`CvGV`), which an import or a glob alias does not
/// change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    pub package: String,
    pub name: String,
    /// `CvANON`: an anonymous sub (`__ANON__`) assigned to a glob
    pub anon: bool,
}

/// The [`Home`] of `cv`; None when it has no name or its stash is gone.
///
/// A `CvNAMED` sub keeps its name as a HEK and its package in
/// `CvSTASH`; these are read as they are, since the `CvGV` macro would
/// create a GV for such a sub (changing the symbol table).
pub fn cv_home(cv: libperl_rs::Cv) -> Option<Home> {
    use libperl_sys as sys;
    unsafe {
        let body = (*cv.as_ptr()).sv_any as *const sys::XPVCV;
        if body.is_null() {
            return None;
        }
        let flags = (*body).xcv_flags;
        let anon = flags & sys::CVf_ANON != 0;
        let (package, name) = if flags & sys::CVf_NAMED != 0 {
            let hek = (*body).xcv_gv_u.xcv_hek;
            let stash = (*body).xcv_stash;
            if hek.is_null() || stash.is_null() {
                return None;
            }
            let key = std::slice::from_raw_parts(
                (&raw const (*hek).hek_key) as *const u8,
                (*hek).hek_len as usize,
            );
            (
                libperl_rs::Hv::from_raw_unchecked(stash).name()?,
                String::from_utf8_lossy(key).into_owned(),
            )
        } else {
            let gv = libperl_rs::Gv::from_raw((*body).xcv_gv_u.xcv_gv)?;
            (gv.stash_name()?, gv.name()?)
        };
        Some(Home {
            package,
            name,
            anon,
        })
    }
}
