//! Perl 公式 API の薄い適応層。
//!
//! 原則: libperl-macrogen が公式 C マクロから生成した Rust 関数
//! (libperl_sys::{PadnamelistMAX, CvROOT, SvTYPE, CopFILE, OpSIBLING, ...})
//! を使う。手書きの構造体アクセスは生成されないものに限る:
//!   - `op_first`: 公開マクロが無い (cUNOPx(o)->op_first 相当)
//!   - `op_name`: OP_NAME は libperl-sys/skip-codegen.txt 掲載のため
//!     PL_op_name 配列参照で代替 (B モジュールと同じ方法)
//!   - `PadnameFLAGS`: macrogen が引数型不明でスキップ (生成側の改良候補)
//!   - METHOP の meth_sv / LOGOP の op_other / LOOP の分岐先:
//!     公開マクロが無い (capture.rs 側で構造体参照)
//!
//! このレイヤの役割は null ガードと Rust らしい型 (bool / Option<String>)
//! への変換のみ。

#![allow(non_snake_case)]

use libperl_sys as sys;
use libperl_sys::{
    HEK, HV, OP, OPf_KIDS, PADLIST, PADNAME, PADNAMELIST, PL_op_name, SV, cop, cv, op, padname,
    sv, svtype, unop,
};

fn cstr_opt(p: *const std::os::raw::c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(
            unsafe { std::ffi::CStr::from_ptr(p) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

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

// ---- CV ----
// (coderef → CV の deref は libperl_rs::Cv::from_coderef / #[xs_sub] の
//  Cv 引数種別へ upstream 済み)

pub fn CvISXSUB(cv: *const cv) -> bool {
    unsafe { sys::CvISXSUB(cv) != 0 }
}

/// XSUB では xcv_root_u が別用途の union のため、先に CvISXSUB を確認する
pub fn CvSTART(cv: *const cv) -> *const op {
    if CvISXSUB(cv) {
        std::ptr::null()
    } else {
        unsafe { sys::CvSTART(cv) }
    }
}

pub fn CvROOT(cv: *const cv) -> *const op {
    if CvISXSUB(cv) {
        std::ptr::null()
    } else {
        unsafe { sys::CvROOT(cv) }
    }
}

pub fn CvFILE(cv: *const cv) -> Option<String> {
    cstr_opt(unsafe { sys::CvFILE(cv) })
}

pub fn CvPADLIST(cv: *const cv) -> *const PADLIST {
    if CvISXSUB(cv) {
        std::ptr::null()
    } else {
        unsafe { sys::CvPADLIST(cv) }
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

pub fn op_name(o: *const op) -> String {
    op_name_of_type(unsafe { (*o).op_type() })
}

/// 最初の子。公開マクロが無いため cUNOPx(o)->op_first 相当を手書き
pub fn op_first(o: *const op) -> *const op {
    if o.is_null() || (unsafe { (*o).op_flags } as u32 & OPf_KIDS) == 0 {
        std::ptr::null()
    } else {
        unsafe { (*(o as *const unop)).op_first }
    }
}

/// 次の兄弟 (公式 OpSIBLING: op_moresib の判定込み)
pub fn op_sibling(o: *const op) -> *const op {
    unsafe { sys::OpSIBLING(o as *mut OP) }
}

/// 実行順 (op_next チェーン) のイテレータ。op_next に公開マクロは無く、
/// B モジュールも直接メンバ参照している
pub fn next_iter(op: *const op) -> OpNextIter {
    OpNextIter { op }
}

pub struct OpNextIter {
    op: *const op,
}

impl Iterator for OpNextIter {
    type Item = *const op;

    fn next(&mut self) -> Option<Self::Item> {
        let op = self.op;
        if op.is_null() {
            None
        } else {
            self.op = unsafe { (*op).op_next as *const op };
            Some(op)
        }
    }
}

// ---- COP ----

pub fn CopLINE(c: *const cop) -> u32 {
    unsafe { sys::CopLINE(c) }
}

pub fn CopFILE(c: *const cop) -> Option<String> {
    cstr_opt(unsafe { sys::CopFILE(c) })
}

// ---- PAD ----

pub fn padlist_names(pl: *const PADLIST) -> *const PADNAMELIST {
    if pl.is_null() {
        std::ptr::null()
    } else {
        unsafe { sys::PadlistNAMES(pl) }
    }
}

/// 使用済み最終インデックス (公式 PadnamelistMAX = xpadnl_fill)
pub fn padnamelist_max(pnl: *const PADNAMELIST) -> isize {
    unsafe { sys::PadnamelistMAX(pnl) }
}

pub fn padnamelist_nth(pnl: *const PADNAMELIST, ix: usize) -> *const padname {
    unsafe { *sys::PadnamelistARRAY(pnl).add(ix) }
}

pub fn PadnamePV(pn: *const padname) -> Option<String> {
    let pv = unsafe { sys::PadnamePV(pn as *mut PADNAME) };
    if pv.is_null() {
        None
    } else {
        let len = unsafe { sys::PadnameLEN(pn as *mut PADNAME) };
        let bytes = unsafe { std::slice::from_raw_parts(pv as *const u8, len as usize) };
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// `my Foo $x` の型 stash 名
pub fn PadnameTYPE(pn: *const padname) -> Option<String> {
    let stash = unsafe { sys::PadnameTYPE(pn as *mut PADNAME) };
    if stash.is_null() {
        None
    } else {
        HvNAME(stash)
    }
}

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

// ---- GV / HEK / HV ----

pub fn isGV_with_GP(sv: *const sv) -> bool {
    !sv.is_null() && unsafe { sys::isGV_with_GP(sv) }
}

pub fn GvNAME_HEK(gv: *const sv) -> *const HEK {
    unsafe { sys::GvNAME_HEK(gv) }
}

pub fn GvSTASH(gv: *const sv) -> *const HV {
    unsafe { sys::GvSTASH(gv) }
}

pub fn HEK_KEY(hek: *const HEK) -> String {
    if hek.is_null() {
        String::new()
    } else {
        cstr_opt(unsafe { sys::HEK_KEY(hek) }).unwrap_or_default()
    }
}

pub fn HvNAME(hv: *const HV) -> Option<String> {
    if hv.is_null() {
        None
    } else {
        cstr_opt(unsafe { sys::HvNAME(hv) })
    }
}
