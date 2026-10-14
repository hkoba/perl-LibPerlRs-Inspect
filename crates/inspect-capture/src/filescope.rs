//! Top-level lexicals of files loaded by `require`.
//!
//! The top level of a required file is compiled into the CV of an eval
//! frame, which is freed once the file has run, so after `perl_parse`
//! (or after the `require` returns) its pad is gone. With [`enable`], the
//! `require` wrapper of [`reqtrace`](crate::reqtrace) looks at each file
//! right after `pp_require` has compiled it — the eval frame is pushed
//! and its ops (`PL_eval_root`) are built, but none of them has run — and
//! keeps the named pad entries, placed in the source the same way as a
//! sub's (see [`capture_sub`](crate::capture_sub)).
//!
//! Only the pad entries are kept (the op tree copy is dropped), keyed by
//! the file name perl compiled the file under (its CopFILE: the path
//! found in `@INC`, or the name given to `require` for a path). A file
//! compiled twice keeps the later one.
//!
//! Limits: the same as `reqtrace` — only `require` ops built after
//! [`enable`] are seen, so enable it before `perl_parse`. Entries are kept
//! per OS thread.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use inspect_core::ir::{OpDetail, OpNode, PadEntry};
use libperl_rs::{Cv, Perl};
use libperl_sys as sys;
use libperl_sys::{PerlInterpreter, thx};

thread_local! {
    static FILE_PADS: RefCell<HashMap<String, Vec<PadEntry>>> = RefCell::new(HashMap::new());
}

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Start keeping the top-level pad of every file `require` compiles
/// from now on.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
    crate::reqtrace::install();
}

pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
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
        let ir = crate::capture::capture_tree(&perl, cv, root, sys::PL_eval_start!(my_perl));
        let Some(file) = first_cop_file(&ir.root) else {
            return;
        };
        FILE_PADS.with(|m| m.borrow_mut().insert(file, ir.pad));
    }
}

/// File of the first COP in tree order
fn first_cop_file(node: &OpNode) -> Option<String> {
    if let OpDetail::Cop { file: Some(f), .. } = &node.detail {
        return Some(f.clone());
    }
    node.kids.iter().find_map(first_cop_file)
}
