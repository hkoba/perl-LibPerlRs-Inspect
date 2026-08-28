//! BEGIN 帰属による `use` 抽出 — perl-LibPerlRs-PartialEval compile.rs の
//! 技法 (差分窓 + CvFILE 完全一致 + B::Deparse::begin_is_use 逆変換) の
//! **ファイル compile 版**。roadmap
//! (libperl-rs/docs/plan/roadmap-next-projects-2026-08.md §3.2) の M2。
//!
//! 手順:
//!   1. `perl_parse` の**前**に [`enable_begin_capture`]
//!      (`PL_savebegin = TRUE`) — parse 中に走った BEGIN CV が
//!      `PL_beginav_save` に退避されるようになる
//!   2. parse 後に [`file_begins`]: CvFILE が対象ファイルに**完全一致**
//!      する BEGIN だけ選び (推移的にロードされた他モジュールの BEGIN を
//!      除外)、埋め込みインタプリタ内の `B::Deparse::begin_is_use`
//!      (非公開だが長年安定 — PartialEval と同じ判断) で
//!      `use Foo (...);` 文へ逆変換する
//!
//! PartialEval の `Handle` と違い refcount の retain/release はしない:
//! 使い捨てインタプリタ前提で、CV は `PL_beginav_save` 自身が生かして
//! いる。既知の制限も PartialEval と同じ — `#line` は CvFILE を変えるので
//! 帰属から漏れる。`begin_is_use` が `""` を返す hint-bit pragma
//! (strict / warnings / feature 等) は uses に載せない (COP hints 側に
//! 焼き付いている)。行番号 0 の BEGIN (sitecustomize / `-Mmodule` 等、
//! ソース 1 行目より前の注入) はソース由来でないため除外する。
//!
//! 注意: [`file_begins`] は B / B::Deparse をインタプリタに require する
//! ため symbol table を汚染する。stash walk を先に済ませてから呼ぶこと。

use libperl_rs::{Cv, Perl, perl_call};
use libperl_sys as sys;

/// 逆変換できた `use`/`no` 文 1 つ。
pub struct FileUse {
    /// 文の行番号 (BEGIN 本体の最初の COP)。
    pub line: Option<u32>,
    /// `use Foo ('a', 'b');` 形の文 (末尾改行は除去済み)。
    pub stmt: String,
}

/// [`file_begins`] の結果: `use` 文と、`use` 形でない生の BEGIN
/// ブロックの行番号。
pub struct FileBegins {
    pub uses: Vec<FileUse>,
    pub opaque_begins: Vec<Option<u32>>,
}

/// `PL_savebegin` を立てる。**`perl_parse` より前に**呼ぶこと。
pub fn enable_begin_capture(perl: &Perl) {
    let my_perl = perl.as_ptr();
    #[cfg(perl_useithreads)]
    unsafe {
        (*my_perl).Isavebegin = true;
    }
    #[cfg(not(perl_useithreads))]
    unsafe {
        let _ = my_perl;
        sys::PL_savebegin = true;
    }
}

/// parse 済みインタプリタから、`file` (通常は `$0`) に帰属する BEGIN 群を
/// `use` 文 / opaque に分類して返す。
pub fn file_begins(perl: &Perl, file: &str) -> Result<FileBegins, String> {
    let my_perl = perl.as_ptr();

    // 1. PL_beginav_save から CvFILE 完全一致の BEGIN を選ぶ
    let mut cvs: Vec<Cv> = Vec::new();
    unsafe {
        let av = sys::PL_beginav_save!(my_perl);
        if !av.is_null() {
            // 内部 AV (magic なし) — AvFILLp+1 個を直接読む
            let n = sys::AvFILLp(av) + 1;
            for i in 0..n {
                let elem = *sys::AvARRAY(av).add(i as usize);
                if elem.is_null() {
                    continue;
                }
                let Some(cv) = Cv::from_raw(elem as *mut sys::CV) else {
                    continue;
                };
                if cv.file().as_deref() == Some(file) {
                    cvs.push(cv);
                }
            }
        }
    }

    // 行番号 (最初の COP)。line 0 の BEGIN は「ソースの 1 行目より前に
    // インタプリタ/起動側が注入したもの」なので除外する — 実例:
    // USE_SITECUSTOMIZE ビルド (Fedora 等) が全プログラムに差し込む
    // `BEGIN { do ".../sitecustomize.pl" if -f ... }`、および `-Mmodule`
    // 由来の use (どちらも CvFILE は本体 file 名、CopLINE は 0 になる)。
    let mut lines: Vec<Option<u32>> = Vec::with_capacity(cvs.len());
    let mut kept: Vec<Cv> = Vec::with_capacity(cvs.len());
    for cv in cvs {
        let line = cv.first_cop(perl).map(|c| c.line());
        if line == Some(0) {
            continue;
        }
        lines.push(line);
        kept.push(cv);
    }
    let cvs = kept;
    let stmts = begin_stmts(perl, &cvs)?;

    // 2. begin_is_use の 3 値 (文 / "" / undef) で分類
    let mut uses = Vec::new();
    let mut opaque_begins = Vec::new();
    for (i, s) in stmts.into_iter().enumerate() {
        match s {
            Some(stmt) if !stmt.is_empty() => uses.push(FileUse {
                line: lines[i],
                stmt: stmt.trim_end_matches('\n').to_string(),
            }),
            Some(_) => {} // hint-bit pragma: COP hints 持ちなので載せない
            None => opaque_begins.push(lines[i]),
        }
    }
    Ok(FileBegins { uses, opaque_begins })
}

/// BEGIN CV 群を埋め込みインタプリタ内で `begin_is_use` に通し、
/// CV ごとに Some(文) (hint pragma は Some("")) / None (opaque) を返す。
fn begin_stmts(perl: &Perl, cvs: &[Cv]) -> Result<Vec<Option<String>>, String> {
    if cvs.is_empty() {
        return Ok(Vec::new());
    }
    let my_perl = perl.as_ptr();
    unsafe {
        // 対象 CV への coderef を作業用パッケージ配列に積む
        let av = perl_call!(
            my_perl,
            Perl_get_av(
                c"LibPerlRs::Inspect::_cli::begins".as_ptr(),
                sys::GV_ADD as i32,
            )
        );
        for cv in cvs {
            // newRV は referent を inc する flavor。返る RV (refcnt 1) の
            // 所有権はそのまま av_push へ移す
            let rv = perl_call!(my_perl, Perl_newRV(cv.as_ptr() as *mut sys::SV));
            perl_call!(my_perl, Perl_av_push(av, rv));
        }

        // 逆変換は Perl 側で。undef は "U"、それ以外は "S" 前置で
        // 文字列化して @stmts に並べる (undef と "" の区別を保つ)
        let code = c"{
            package LibPerlRs::Inspect::_cli;
            our (@begins, @stmts);
            require B; require B::Deparse;
            my $d = B::Deparse->new;
            @stmts = map {
                my $s = eval { $d->begin_is_use(B::svref_2object($_)) };
                defined($s) ? \"S$s\" : \"U\";
            } @begins;
            1;
        }";
        perl_call!(my_perl, Perl_eval_pv(code.as_ptr(), 0));
        if let Some(err) = errsv(perl) {
            return Err(format!("begin_is_use failed: {err}"));
        }

        let sav = perl_call!(
            my_perl,
            Perl_get_av(
                c"LibPerlRs::Inspect::_cli::stmts".as_ptr(),
                sys::GV_ADD as i32,
            )
        );
        let n = sys::AvFILLp(sav) + 1;
        if n as usize != cvs.len() {
            return Err(format!(
                "begin_is_use produced {n} results for {} BEGINs",
                cvs.len()
            ));
        }
        let mut out = Vec::with_capacity(cvs.len());
        for i in 0..n {
            let elem = *sys::AvARRAY(sav).add(i as usize);
            let s = if elem.is_null() {
                String::new()
            } else {
                let sv = libperl_rs::Sv::from_raw_unchecked(elem);
                String::from_utf8_lossy(sv.pv(perl)).into_owned()
            };
            out.push(match s.strip_prefix('S') {
                Some(stmt) => Some(stmt.to_string()),
                None => None, // "U" = undef = use 形でない BEGIN
            });
        }
        Ok(out)
    }
}

/// $@ が非空なら Some(メッセージ)。
fn errsv(perl: &Perl) -> Option<String> {
    let msg_sv = perl.get_sv("@", 0)?;
    let msg = msg_sv.pv(perl);
    if msg.is_empty() {
        None
    } else {
        Some(String::from_utf8_lossy(msg).into_owned())
    }
}
