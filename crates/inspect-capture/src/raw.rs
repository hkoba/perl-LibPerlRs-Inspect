//! Thin adaptation layer over the official Perl API — holds **only the
//! remainder** not provided by the libperl-rs 0.4.4 introspection layer
//! (Op / Cop / Gv / PadName newtypes):
//!   - `SvTYPE` / `SvROK` / `SvRV`: pass-through wrappers on raw pointers
//!     (`*const sv`), used by literal extraction (sv_lit in capture.rs)
//!   - `op_name_of_type`: OP_NAME is listed in libperl-sys/skip-codegen.txt,
//!     so substitute a PL_op_name array lookup (same method as the B module)
//!   - `PadnameFLAGS`: skipped by macrogen because the argument type is
//!     unknown (candidate improvement on the generator side)
//!   - `PAD_BASE_SV`: pad slot resolution equivalent to the perl-internal
//!     macro (not provided by libperl-rs — next upstream candidate)
//!   - METHOP's meth_sv / LOGOP's op_other / LOOP branch targets: no public
//!     macros exist (accessed via struct fields in capture.rs)

#![allow(non_snake_case)]

use libperl_sys as sys;
use libperl_sys::{PADLIST, PL_op_name, SV, padname, sv, svtype};

// ---- SV ----

pub fn SvTYPE(sv: *const sv) -> svtype {
    unsafe { sys::SvTYPE(sv) }
}

pub fn SvROK(sv: *const sv) -> bool {
    unsafe { sys::SvROK(sv) != 0 }
}

pub fn SvRV(sv: *const sv) -> *const sv {
    if SvROK(sv) {
        unsafe { sys::SvRV(sv) }
    } else {
        std::ptr::null()
    }
}

// ---- OP ----

/// OP_NAME is listed in skip-codegen.txt, so substitute PL_op_name
pub fn op_name_of_type(ty: u16) -> String {
    unsafe { std::ffi::CStr::from_ptr(PL_op_name[ty as usize]) }
        .to_str()
        .unwrap()
        .to_string()
}

// ---- PAD ----

/// Hand-written because macrogen skips generating it (commented out in
/// macro_bindings.rs). Candidate improvement on the generator side
pub fn PadnameFLAGS(pn: *const padname) -> u8 {
    unsafe { (*pn).xpadn_flags }
}

/// Equivalent of the perl-internal macro PAD_BASE_SV: PadlistARRAY(pl)[1] is
/// the actual pad (AV); return its po-th element. Out-of-range po returns null
pub fn PAD_BASE_SV(pl: *const PADLIST, po: isize) -> *const SV {
    if pl.is_null() || po < 0 {
        return std::ptr::null();
    }
    let pad = unsafe { *sys::PadlistARRAY(pl).add(1) };
    if pad.is_null() || po > unsafe { sys::PadMAX(pad) } {
        return std::ptr::null();
    }
    unsafe { *sys::PadARRAY(pad).add(po as usize) }
}
