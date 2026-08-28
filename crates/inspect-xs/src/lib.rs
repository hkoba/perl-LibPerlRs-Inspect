//! LibPerlRs::Inspect — XS glue crate.
//!
//! Loaded from Perl via XSLoader as `auto/LibPerlRs/Inspect/Analyzer.so`.
//! coderef 引数は `#[xs_sub]` の `Cv` 種別 (libperl-rs) がトランポリンで
//! 検査する。`analyze` はネイティブの hashref を返し、`*_json` 系は
//! スキーマ確認やデバッグ用に JSON 文字列を返す。

use libperl_rs::{Av, Cv, Hv, Perl, Rv, Sv, sv_undef_ptr, xs_boot, xs_sub};
use serde_json::Value;

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

/// serde_json::Value → Perl データ構造。レポートの中間表現として
/// Value を使うことで、JSON 版とネイティブ版が常に同じ形になる。
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
    subs = [op_names_json, capture_json, dump_optree, analyze_json, analyze];
}
