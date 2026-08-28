//! Perl 公式 API の薄い適応層 — libperl-rs 0.5 (Step 2 introspection
//! 層) が提供しない**残余だけ**を持つ。
//!
//! かつてここにあった OpNextIter / op_first / op_sibling / CopLINE /
//! CopFILE / Gv・Padname 系の適応は libperl-rs の newtype 層
//! (Op / Cop / Gv / PadName — 旧 backlog #3 の upstream) に置き換えた。
//! 残っているのは:
//!   - `SvTYPE` / `SvROK` / `SvRV`: 生ポインタ (`*const sv`) のまま
//!     リテラル抽出 (capture.rs の sv_lit) が使う素通しラッパ
//!   - `op_name_of_type`: OP_NAME は libperl-sys/skip-codegen.txt 掲載の
//!     ため PL_op_name 配列参照で代替 (B モジュールと同じ方法)
//!   - `PadnameFLAGS`: macrogen が引数型不明でスキップ (生成側の改良候補)
//!   - `PAD_BASE_SV`: perl 内部マクロ相当の pad slot 解決
//!     (libperl-rs 未提供 — 次の upstream 候補)
//!   - METHOP の meth_sv / LOGOP の op_other / LOOP の分岐先:
//!     公開マクロが無い (capture.rs 側で構造体参照)

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

/// OP_NAME は skip-codegen.txt 掲載のため PL_op_name で代替
pub fn op_name_of_type(ty: u16) -> String {
    unsafe { std::ffi::CStr::from_ptr(PL_op_name[ty as usize]) }
        .to_str()
        .unwrap()
        .to_string()
}

// ---- PAD ----

/// macrogen が生成をスキップしている (macro_bindings.rs でコメントアウト)
/// ため手書き。生成側の改良候補
pub fn PadnameFLAGS(pn: *const padname) -> u8 {
    unsafe { (*pn).xpadn_flags }
}

/// perl 内部マクロ PAD_BASE_SV 相当: PadlistARRAY(pl)[1] が実 pad (AV)、
/// その po 番目。範囲外の po は null を返す
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
