//! Top-level lexicals of files loaded by `require`.
//!
//! The top level of a required file is compiled into the CV of an eval
//! frame, which is freed once the file has run, so after `perl_parse`
//! (or after the `require` returns) its pad is gone. Once enabled, the
//! `require` wrapper of [`reqtrace`](crate::reqtrace) looks at each file
//! right after `pp_require` has compiled it — the eval frame is pushed
//! and its ops (`PL_eval_root`) are built, but none of them has run — and
//! keeps the named pad entries, placed in the source the same way as a
//! sub's (see [`capture_sub`](crate::capture_sub)).
//!
//! Files are named by the path `require` read them from: their `%INC`
//! value (the name given to `require` for a path, otherwise the path
//! found in `@INC`), which a `#line` directive in the file does not
//! change. [`want`] limits the capture to the given files; this is the
//! recommended use, since the op tree of every other file is then left
//! alone. [`enable`] captures every file. Only the pad entries are kept
//! (the op tree copy is dropped); a file compiled twice keeps the later
//! one.
//!
//! Limits: the same as `reqtrace` — only `require` ops built after
//! [`enable`] / [`want`] are seen, so call them before the code that
//! requires the file is compiled (before `perl_parse`, or before an
//! `eval_pv` that requires it). Entries and targets are kept per OS
//! thread.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use inspect_core::ir::PadEntry;
use libperl_rs::{Cv, Perl, Sv};
use libperl_sys as sys;
use libperl_sys::{PerlInterpreter, thx};

/// Which files to capture
enum Targets {
    All,
    Only(HashSet<String>),
}

thread_local! {
    static TARGETS: RefCell<Targets> = RefCell::new(Targets::Only(HashSet::new()));
    static FILE_PADS: RefCell<HashMap<String, Vec<PadEntry>>> = RefCell::new(HashMap::new());
}

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Keep the top-level pad of every file `require` compiles from now on.
pub fn enable() {
    TARGETS.with(|t| *t.borrow_mut() = Targets::All);
    install();
}

/// Keep the top-level pad of `file` when `require` compiles it from now
/// on. `file` is the path as `require` gets it (its `%INC` value), e.g.
/// the absolute path given to `require $path`. Can be called several
/// times, also after `perl_parse`; no effect after [`enable`].
pub fn want(file: &str) {
    TARGETS.with(|t| {
        if let Targets::Only(set) = &mut *t.borrow_mut() {
            set.insert(file.to_string());
        }
    });
    install();
}

fn install() {
    ENABLED.store(true, Ordering::Relaxed);
    crate::reqtrace::install();
}

pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn wanted(file: &str) -> bool {
    TARGETS.with(|t| match &*t.borrow() {
        Targets::All => true,
        Targets::Only(set) => set.contains(file),
    })
}

/// The top-level pad entries of `file`, as compiled by a `require` while
/// enabled; None when no such compile was seen.
pub fn file_pad(file: &str) -> Option<Vec<PadEntry>> {
    FILE_PADS.with(|m| m.borrow().get(file).cloned())
}

/// Called after the original `pp_require` returned; `cxix` is
/// `cxstack_ix` from before it. A new `require` eval frame on top means
/// a file has just been compiled and is about to run.
pub(crate) unsafe fn after_require(my_perl: *mut PerlInterpreter, cxix: i32) {
    unsafe {
        let top = crate::reqtrace::cx_index(my_perl);
        if top <= cxix {
            return; // cached, `require VERSION`, or no new frame
        }
        let si = sys::PL_curstackinfo!(my_perl);
        let cx = (*si).si_cxstack.add(top as usize);
        if thx::CxTYPE(my_perl, cx) as u32 != sys::CXt_EVAL
            || thx::CxOLD_OP_TYPE(my_perl, cx) != sys::opcode::OP_REQUIRE as _
        {
            return;
        }
        let Some(cv) = Cv::from_raw((*cx).cx_u.cx_blk.blk_u.blku_eval.cv) else {
            return;
        };
        let root = sys::PL_eval_root!(my_perl);
        if root.is_null() {
            return;
        }
        let perl = std::mem::ManuallyDrop::new(Perl::from_raw_unchecked(my_perl));
        let Some(file) = inc_value(&perl, (*cx).cx_u.cx_blk.blk_u.blku_eval.old_namesv) else {
            return;
        };
        if !wanted(&file) {
            return;
        }
        let ir = crate::capture::capture_tree(&perl, cv, root, sys::PL_eval_start!(my_perl));
        FILE_PADS.with(|m| m.borrow_mut().insert(file, ir.pad));
    }
}

/// `$INC{name}` for the `%INC` key `namesv`, set by `require` before it
/// compiles the file
unsafe fn inc_value(perl: &Perl, namesv: *mut sys::SV) -> Option<String> {
    unsafe {
        if namesv.is_null() {
            return None;
        }
        let key = Sv::from_raw_unchecked(namesv).pv(perl).to_vec();
        let inc = thx::Perl_get_hv(perl.as_ptr(), c"INC".as_ptr(), 0);
        if inc.is_null() {
            return None;
        }
        let svp = thx::Perl_hv_fetch(
            perl.as_ptr(),
            inc,
            key.as_ptr() as *const _,
            key.len() as _,
            0,
        );
        if svp.is_null() || (*svp).is_null() || thx::SvOK(perl.as_ptr(), *svp) == 0 {
            return None;
        }
        Some(String::from_utf8_lossy(Sv::from_raw_unchecked(*svp).pv(perl)).into_owned())
    }
}
