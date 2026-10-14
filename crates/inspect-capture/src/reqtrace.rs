//! Run-time `require` tracing by wrapping `PL_ppaddr[OP_REQUIRE]`.
//!
//! Every `require` (and so every `use`) executed while tracing is on is
//! recorded with the requiring location and the files being loaded at
//! that moment. Unlike an `@INC` hook, this also sees the requires that
//! are no-ops because the file is already in `%INC` — including the one
//! that closes a dependency cycle while its target is still compiling
//! (`in_progress`).
//!
//! The loading stack comes from the context stack: perl runs each file
//! loaded by `require` inside an eval frame (`CXt_EVAL` whose
//! `CxOLD_OP_TYPE` is `OP_REQUIRE`) that carries the file name in
//! `old_namesv`. BEGIN blocks run on their own stackinfo, so the whole
//! `si_prev` chain is walked.
//!
//! Limits:
//!   - An op copies its pp function from `PL_ppaddr` when it is built, so
//!     only `require` ops compiled after [`start`] are traced (the ones
//!     `use` compiles later, and those of files loaded later). Start
//!     tracing before loading the code to inspect.
//!   - `PL_ppaddr` is process-global; events are kept per OS thread.
//!   - `do FILE` is not traced.
//!   - The file a `load` will read is found by searching `@INC` again
//!     (`@INC` hooks are skipped), and a reload is detected by comparing
//!     canonical paths with `$0` and the other `%INC` values.

use std::cell::RefCell;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use libperl_rs::{Av, Cop, Cv, Gv, Hv, Perl, Sv, SvKind};
use libperl_sys as sys;
use libperl_sys::{OP, PerlInterpreter, thx};

/// One executed `require`.
#[derive(Debug, Clone)]
pub struct RequireEvent {
    /// The `%INC` key (`Foo/Bar.pm`, or the string of `require "x.pl"`)
    pub name: String,
    /// Package name for a `*.pm` file
    pub module: Option<String>,
    /// Location of the `require` (for `use`, the `use` statement)
    pub from_file: Option<String>,
    pub from_line: u32,
    /// `load`: not in `%INC`, loading starts now. `cached`: already
    /// loaded. `in_progress`: in `%INC` but still being loaded (a
    /// compile-time cycle). `failed`: an earlier load failed.
    pub status: &'static str,
    /// Files being loaded at that moment, outermost first
    pub stack: Vec<String>,
    /// For `load`: the file that will be read, found by searching `@INC`
    /// the way perl does (plain directory entries only)
    pub path: Option<String>,
    /// For `load`: the file at `path` has been compiled already, as the
    /// main program (`$0`) or under this other `%INC` key — loading it
    /// again compiles the same code twice
    pub reload_of: Option<String>,
    /// For `in_progress` and reloads: what the target package provided at
    /// that moment (what the requiring side gets to see)
    pub snapshot: Option<PackageSnapshot>,
}

/// State of a package at one moment of its compilation.
#[derive(Debug, Clone)]
pub struct PackageSnapshot {
    pub package: String,
    /// Names of the subs defined so far, imported ones included
    /// (declared-only subs and method cache entries excluded)
    pub subs: Vec<String>,
    /// The `import` method resolution finds, by its qualified name
    /// (`Exporter::import`, `Foo::import`, `UNIVERSAL::import`, ...);
    /// None when there is none
    pub import: Option<String>,
    pub isa: Vec<String>,
    pub export: Vec<String>,
    pub export_ok: Vec<String>,
}

thread_local! {
    static EVENTS: RefCell<Vec<RequireEvent>> = const { RefCell::new(Vec::new()) };
}

/// The pp function [`start`] replaced. Kept after [`stop`]: ops compiled
/// while tracing hold [`traced_require`] and still call through it.
static mut ORIG: sys::Perl_ppaddr_t = None;

/// Whether [`traced_require`] records events
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Address of the wrapper as written into the slot (0 = never installed);
/// compared by value since fn pointer identity is not guaranteed
static INSTALLED: AtomicUsize = AtomicUsize::new(0);

fn slot_addr(p: sys::Perl_ppaddr_t) -> usize {
    p.map_or(0, |f| f as usize)
}

/// Start tracing: clear the recorded events and install the wrapper.
/// Starting again while already tracing only clears the events.
pub fn start() {
    EVENTS.with(|e| e.borrow_mut().clear());
    unsafe {
        let slot = &raw mut sys::PL_ppaddr[sys::opcode::OP_REQUIRE as usize];
        let installed = INSTALLED.load(Ordering::Relaxed);
        if installed == 0 || slot_addr(*slot) != installed {
            let orig = ORIG;
            if orig.is_none() {
                ORIG = *slot;
            }
            let wrapper: sys::Perl_ppaddr_t = Some(traced_require);
            *slot = wrapper;
            INSTALLED.store(slot_addr(wrapper), Ordering::Relaxed);
        }
    }
    ACTIVE.store(true, Ordering::Relaxed);
}

/// Stop tracing. The events are kept. Ops compiled while tracing keep
/// calling the wrapper, which then passes straight through.
pub fn stop() {
    ACTIVE.store(false, Ordering::Relaxed);
    unsafe {
        let slot = &raw mut sys::PL_ppaddr[sys::opcode::OP_REQUIRE as usize];
        // leave the slot alone if someone else replaced the wrapper since
        if slot_addr(*slot) == INSTALLED.load(Ordering::Relaxed) {
            *slot = ORIG;
        }
    }
}

/// The events recorded on this thread since the last [`start`].
pub fn events() -> Vec<RequireEvent> {
    EVENTS.with(|e| e.borrow().clone())
}

#[cfg(perl_useithreads)]
unsafe extern "C" fn traced_require(my_perl: *mut PerlInterpreter) -> *mut OP {
    unsafe {
        if ACTIVE.load(Ordering::Relaxed) {
            let _ = std::panic::catch_unwind(|| record(my_perl));
        }
        // ORIG is set before the wrapper is ever installed
        let Some(orig) = ORIG else {
            std::process::abort()
        };
        orig(my_perl)
    }
}

#[cfg(not(perl_useithreads))]
unsafe extern "C" fn traced_require() -> *mut OP {
    unsafe {
        if ACTIVE.load(Ordering::Relaxed) {
            let _ = std::panic::catch_unwind(|| record(std::ptr::null_mut()));
        }
        // ORIG is set before the wrapper is ever installed
        let Some(orig) = ORIG else {
            std::process::abort()
        };
        orig()
    }
}

/// Record the require about to run. The argument is on top of the stack.
unsafe fn record(my_perl: *mut PerlInterpreter) {
    unsafe {
        let perl = std::mem::ManuallyDrop::new(Perl::from_raw_unchecked(my_perl));
        let sp = sys::PL_stack_sp!(my_perl);
        if sp.is_null() || (*sp).is_null() {
            return;
        }
        let arg = *sp;
        // `require VERSION` loads nothing
        if thx::SvNIOKp(my_perl, arg) != 0 || thx::SvVOK(my_perl, arg) {
            return;
        }
        let name_bytes = Sv::from_raw_unchecked(arg).pv(&perl).to_vec();
        let name = String::from_utf8_lossy(&name_bytes).into_owned();

        let stack = loading_stack(&perl);
        let status = {
            let inc = thx::Perl_get_hv(my_perl, c"INC".as_ptr(), 0);
            let svp = if inc.is_null() {
                std::ptr::null_mut()
            } else {
                thx::Perl_hv_fetch(
                    my_perl,
                    inc,
                    name_bytes.as_ptr() as *const _,
                    name_bytes.len() as _,
                    0,
                )
            };
            if svp.is_null() {
                "load"
            } else if (*svp).is_null() || thx::SvOK(my_perl, *svp) == 0 {
                "failed"
            } else if stack.contains(&name) {
                "in_progress"
            } else {
                "cached"
            }
        };
        let (path, reload_of) = if status == "load" {
            let path = resolve_path(&perl, &name);
            let reload_of = path.as_deref().and_then(|p| compiled_as(&perl, &name, p));
            (path, reload_of)
        } else {
            (None, None)
        };
        let module = inspect_core::passes::deps::module_of_file(&name);
        let snapshot = if status == "in_progress" || reload_of.is_some() {
            module.as_deref().map(|m| snapshot(&perl, m))
        } else {
            None
        };
        let cop = Cop::from_raw(sys::PL_curcop!(my_perl));
        let ev = RequireEvent {
            module,
            name,
            from_file: cop.as_ref().and_then(|c| c.file()),
            from_line: cop.map(|c| c.line()).unwrap_or(0),
            status,
            stack,
            path,
            reload_of,
            snapshot,
        };
        EVENTS.with(|e| e.borrow_mut().push(ev));
    }
}

/// Names of the files being loaded by `require`, outermost first.
fn loading_stack(perl: &Perl) -> Vec<String> {
    let my_perl = perl.as_ptr();
    let mut infos = Vec::new();
    unsafe {
        let mut si = sys::PL_curstackinfo!(my_perl);
        while !si.is_null() {
            infos.push(si);
            si = (*si).si_prev;
        }
    }
    let mut out = Vec::new();
    for si in infos.into_iter().rev() {
        unsafe {
            let cxs = (*si).si_cxstack;
            for i in 0..=(*si).si_cxix {
                if i < 0 || cxs.is_null() {
                    break;
                }
                let cx = cxs.add(i as usize);
                if thx::CxTYPE(my_perl, cx) as u32 != sys::CXt_EVAL
                    || thx::CxOLD_OP_TYPE(my_perl, cx) != sys::opcode::OP_REQUIRE as _
                {
                    continue;
                }
                let namesv = (*cx).cx_u.cx_blk.blk_u.blku_eval.old_namesv;
                if !namesv.is_null() {
                    let sv = Sv::from_raw_unchecked(namesv);
                    out.push(String::from_utf8_lossy(sv.pv(perl)).into_owned());
                }
            }
        }
    }
    out
}

/// The file `require` will read for `name`: `name` itself when it is an
/// absolute or `./` / `../` path, otherwise the first `@INC` directory
/// holding it. `@INC` hooks (references) are skipped.
fn resolve_path(perl: &Perl, name: &str) -> Option<String> {
    let explicit =
        Path::new(name).is_absolute() || name.starts_with("./") || name.starts_with("../");
    if explicit {
        return Path::new(name).is_file().then(|| name.to_string());
    }
    let inc = unsafe { thx::Perl_get_av(perl.as_ptr(), c"INC".as_ptr(), 0) };
    if inc.is_null() {
        return None;
    }
    let inc = unsafe { Av::from_raw_unchecked(inc) };
    for sv in inc.iter(perl).flatten() {
        let SvKind::Scalar(sv) = sv.kind() else {
            continue;
        };
        let dir = String::from_utf8_lossy(sv.pv(perl)).into_owned();
        let candidate = Path::new(&dir).join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// Whether the file at `path` was compiled already: as the main program
/// (returns `$0`) or under another `%INC` key (returns that key).
/// Files are compared by canonical path.
fn compiled_as(perl: &Perl, name: &str, path: &str) -> Option<String> {
    let target = std::fs::canonicalize(path).ok()?;
    let same = |p: &str| std::fs::canonicalize(p).is_ok_and(|c| c == target);
    if let Some(sv0) = perl.get_sv("0", 0) {
        let prog = String::from_utf8_lossy(sv0.pv(perl)).into_owned();
        if same(&prog) {
            return Some(prog);
        }
    }
    let inc = unsafe { thx::Perl_get_hv(perl.as_ptr(), c"INC".as_ptr(), 0) };
    if inc.is_null() {
        return None;
    }
    let inc = unsafe { Hv::from_raw_unchecked(inc) };
    for (key, val) in inc.iter(perl) {
        let SvKind::Scalar(val) = val.kind() else {
            continue;
        };
        if key == name.as_bytes() || unsafe { thx::SvOK(perl.as_ptr(), val.as_ptr()) } == 0 {
            continue;
        }
        if same(&String::from_utf8_lossy(val.pv(perl))) {
            return Some(String::from_utf8_lossy(key).into_owned());
        }
    }
    None
}

/// What `package` provides right now: defined subs, the `import` method
/// it resolves to, `@ISA`, `@EXPORT`, `@EXPORT_OK`.
fn snapshot(perl: &Perl, package: &str) -> PackageSnapshot {
    let my_perl = perl.as_ptr();
    let mut snap = PackageSnapshot {
        package: package.to_string(),
        subs: Vec::new(),
        import: None,
        isa: array_strings(perl, &format!("{package}::ISA")),
        export: array_strings(perl, &format!("{package}::EXPORT")),
        export_ok: array_strings(perl, &format!("{package}::EXPORT_OK")),
    };
    let Some(stash) = perl.gv_stashpv(package, 0) else {
        return snap;
    };
    for (key, val) in stash.iter(perl) {
        let cv = match val.kind() {
            // GvCVu: not a method cache entry (GvCVGEN == 0)
            SvKind::Glob(gv) => Cv::from_raw(unsafe { thx::GvCVu(my_perl, gv.as_ptr()) }),
            // simple subs are stored as a reference to the CV
            SvKind::Ref(target) => match target.kind() {
                SvKind::Code(cv) => Some(cv),
                _ => None,
            },
            _ => None,
        };
        if cv.is_some_and(|cv| cv.is_xsub() || !cv.root().is_null()) {
            snap.subs.push(String::from_utf8_lossy(key).into_owned());
        }
    }
    snap.subs.sort();
    let gv =
        unsafe { thx::Perl_gv_fetchmeth_pvn(my_perl, stash.as_ptr(), c"import".as_ptr(), 6, 0, 0) };
    snap.import = Gv::from_raw(gv)
        .and_then(|gv| gv.cv())
        .filter(|cv| cv.is_xsub() || !cv.root().is_null())
        .and_then(|cv| cv.names(perl))
        .map(|(pkg, _)| pkg);
    snap
}

/// String elements of the package array `name` (empty when absent).
fn array_strings(perl: &Perl, name: &str) -> Vec<String> {
    let Ok(cname) = std::ffi::CString::new(name) else {
        return Vec::new();
    };
    let av = unsafe { thx::Perl_get_av(perl.as_ptr(), cname.as_ptr(), 0) };
    if av.is_null() {
        return Vec::new();
    }
    let av = unsafe { Av::from_raw_unchecked(av) };
    av.iter(perl)
        .flatten()
        .map(|sv| String::from_utf8_lossy(sv.pv(perl)).into_owned())
        .collect()
}
