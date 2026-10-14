//! LibPerlRs::Inspect — XS glue crate.
//!
//! Loaded from Perl via XSLoader as `auto/LibPerlRs/Inspect/Analyzer.so`.
//! The coderef argument is checked by the trampoline of `#[xs_sub]`'s `Cv`
//! kind (libperl-rs). `analyze` returns a native hashref; the `*_json`
//! variants return JSON strings for schema checks and debugging.

use inspect_core::passes::deps;
use libperl_rs::{Av, Cv, Hv, IV, Perl, Rv, Sv, sv_undef_ptr, xs_boot, xs_sub};
use serde_json::{Value, json};

/// `LibPerlRs::Inspect::op_names_json($coderef)` — op names of the sub's
/// execution-order chain (CvSTART → op_next → …), as a JSON array.
#[xs_sub]
fn op_names_json(code: Cv) -> Result<String, String> {
    let names = inspect_capture::exec_op_names(code)?;
    serde_json::to_string(&names).map_err(|e| e.to_string())
}

/// `LibPerlRs::Inspect::capture_json($coderef)` — full owned IR (SubIr)
/// of the sub's OP tree, as JSON.
#[xs_sub]
fn capture_json(my_perl: &Perl, code: Cv) -> Result<String, String> {
    let ir = inspect_capture::capture_sub(my_perl, code)?;
    serde_json::to_string(&ir).map_err(|e| e.to_string())
}

/// `LibPerlRs::Inspect::dump_optree($coderef)` — human-readable tree dump
/// for debugging.
#[xs_sub]
fn dump_optree(my_perl: &Perl, code: Cv) -> Result<String, String> {
    let ir = inspect_capture::capture_sub(my_perl, code)?;
    Ok(inspect_core::dump::dump(&ir))
}

/// `LibPerlRs::Inspect::analyze_json($coderef)` — analysis report as a
/// JSON string (args / returns / logic / types / lints).
#[xs_sub]
fn analyze_json(my_perl: &Perl, code: Cv) -> Result<String, String> {
    let ir = inspect_capture::capture_sub(my_perl, code)?;
    let report = inspect_core::passes::analyze(&ir);
    serde_json::to_string(&report).map_err(|e| e.to_string())
}

/// `LibPerlRs::Inspect::analyze($coderef)` — analysis report as a native
/// nested hashref (no JSON round-trip on the Perl side).
#[xs_sub]
fn analyze(my_perl: &Perl, code: Cv) -> Result<Rv<Hv>, String> {
    let ir = inspect_capture::capture_sub(my_perl, code)?;
    let report = inspect_core::passes::analyze(&ir);
    let value = serde_json::to_value(&report).map_err(|e| e.to_string())?;
    let Value::Object(map) = value else {
        return Err("analysis report did not serialize to an object".into());
    };
    Ok(build_hv(my_perl, &map).into_rv(my_perl))
}

/// `LibPerlRs::Inspect::begin_capture($on)` — turn BEGIN capture
/// (`PL_savebegin`) on or off. BEGIN blocks (including the ones `use`
/// compiles into) that run while it is on are kept for [`begins`].
#[xs_sub]
fn begin_capture(my_perl: &Perl, on: IV) {
    inspect_capture::set_begin_capture(my_perl, on != 0);
}

/// `LibPerlRs::Inspect::begins()` — arrayref with one hashref per BEGIN
/// saved since capture was turned on, in the order they ran, from every
/// file: `{ file, line, code, kind => use|no|opaque, module, version,
/// args, requires }`.
#[xs_sub]
fn begins(my_perl: &Perl) -> Result<Rv<Av>, String> {
    let av = Av::new(my_perl);
    for cv in inspect_capture::saved_begins(my_perl) {
        let mut m = serde_json::Map::new();
        m.insert("file".into(), json!(cv.file()));
        m.insert(
            "line".into(),
            json!(cv.first_cop(my_perl).map(|c| c.line())),
        );
        match inspect_capture::capture_sub(my_perl, cv) {
            Ok(ir) => {
                match deps::use_form(&ir) {
                    Some(u) => {
                        m.insert("kind".into(), json!(u.kind));
                        m.insert("module".into(), json!(u.module));
                        if let Some(v) = u.version {
                            m.insert("version".into(), json!(v));
                        }
                        if let Some(a) = u.args {
                            m.insert("args".into(), json!(a));
                        }
                    }
                    None => {
                        m.insert("kind".into(), json!("opaque"));
                    }
                }
                let sites =
                    serde_json::to_value(deps::require_sites(&ir)).map_err(|e| e.to_string())?;
                m.insert("requires".into(), sites);
            }
            Err(e) => {
                m.insert("kind".into(), json!("opaque"));
                m.insert("error".into(), json!(e));
            }
        }
        let hv = build_hv(my_perl, &m);
        hv.store(my_perl, "code", code_ref(my_perl, cv));
        av.push(my_perl, hv.into_rv(my_perl).as_sv());
    }
    Ok(av.into_rv(my_perl))
}

/// `LibPerlRs::Inspect::require_sites($coderef)` — arrayref of the
/// static `require` ops of a sub: `{ line, file, module, version,
/// dynamic }`.
#[xs_sub]
fn require_sites(my_perl: &Perl, code: Cv) -> Result<Rv<Av>, String> {
    let ir = inspect_capture::capture_sub(my_perl, code)?;
    let value = serde_json::to_value(deps::require_sites(&ir)).map_err(|e| e.to_string())?;
    let Value::Array(items) = value else {
        return Err("require sites did not serialize to an array".into());
    };
    let av = Av::new(my_perl);
    for item in &items {
        av.push(my_perl, json_to_sv(my_perl, item));
    }
    Ok(av.into_rv(my_perl))
}

/// `LibPerlRs::Inspect::packages()` — every package reachable from
/// `main::` (list).
#[xs_sub]
fn packages(my_perl: &Perl) -> Vec<String> {
    inspect_capture::packages::package_names(my_perl)
}

/// `LibPerlRs::Inspect::inheritance($package)` — `{ isa, linear_isa, mro }`
/// for a package with a non-empty `@ISA`, otherwise undef.
#[xs_sub]
fn inheritance(my_perl: &Perl, package: &str) -> Option<Rv<Hv>> {
    let inh = inspect_capture::packages::inheritance(my_perl, package)?;
    let mut m = serde_json::Map::new();
    m.insert("isa".into(), json!(inh.isa));
    m.insert("linear_isa".into(), json!(inh.linear_isa));
    m.insert("mro".into(), json!(inh.mro));
    Some(build_hv(my_perl, &m).into_rv(my_perl))
}

/// A new reference to `cv` (\&sub). newRV increments the referent.
fn code_ref(perl: &Perl, cv: Cv) -> Sv {
    let my_perl = perl.as_ptr();
    unsafe {
        let rv = libperl_rs::perl_call!(my_perl, Perl_newRV(cv.as_ptr() as *mut libperl_rs::SV));
        Sv::from_raw_unchecked(rv)
    }
}

/// serde_json::Value → Perl data structure. Using Value as the report's
/// intermediate representation keeps the JSON and native versions always
/// in the same shape.
fn json_to_sv(perl: &Perl, v: &Value) -> Sv {
    match v {
        Value::Null => unsafe { Sv::from_raw_unchecked(sv_undef_ptr(perl.as_ptr())) },
        Value::Bool(b) => Sv::new_iv(perl, *b as i64),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Sv::new_iv(perl, i)
            } else if let Some(u) = n.as_u64() {
                Sv::new_uv(perl, u)
            } else {
                Sv::new_nv(perl, n.as_f64().unwrap_or(f64::NAN))
            }
        }
        Value::String(s) => Sv::new_pv(perl, s),
        Value::Array(items) => {
            let av = Av::new(perl);
            for item in items {
                av.push(perl, json_to_sv(perl, item));
            }
            av.into_rv(perl).as_sv()
        }
        Value::Object(map) => build_hv(perl, map).into_rv(perl).as_sv(),
    }
}

fn build_hv(perl: &Perl, map: &serde_json::Map<String, Value>) -> Hv {
    let hv = Hv::new(perl);
    for (k, v) in map {
        hv.store(perl, k, json_to_sv(perl, v));
    }
    hv
}

xs_boot! {
    package = "LibPerlRs::Inspect";
    subs = [
        op_names_json,
        capture_json,
        dump_optree,
        analyze_json,
        analyze,
        begin_capture,
        begins,
        require_sites,
        packages,
        inheritance,
    ];
}
