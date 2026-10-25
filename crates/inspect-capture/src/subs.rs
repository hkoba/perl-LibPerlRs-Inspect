//! Sub enumeration without a report: every named sub of the interpreter
//! with where it lives, cheap enough to re-run after code has been loaded
//! at run time (string `eval`, `require`).

use libperl_rs::{Cv, Perl, StashWalker, perl_call};

use crate::capture::cv_lines;
use crate::packages::{Home, cv_home};

/// One named sub, as found through the symbol table.
#[derive(Clone)]
pub struct SubInfo {
    /// Package the sub is listed in and its name there.
    pub package: String,
    pub name: String,
    /// CvFILE; `"(eval N)"` or `"(eval N)[file:line]"` (PERLDBf_NAMEEVAL)
    /// for subs compiled by a string eval.
    pub file: Option<String>,
    /// (min line, max line) over the COPs of the body.
    pub lines: Option<(u32, u32)>,
    pub xsub: bool,
    pub proto: Option<String>,
    /// Where the sub was named when that differs from where it is listed
    /// (an import, a glob alias, an anonymous sub assigned to a glob).
    pub home: Option<Home>,
    /// File and line of the glob the sub hangs off, if it has one.
    pub gv: Option<(String, u32)>,
    /// Address of the CV, as an identity for diffing between calls.
    pub cv_addr: usize,
    /// The live CV (valid as long as it stays in the symbol table).
    pub cv: Cv,
}

impl std::fmt::Debug for SubInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubInfo")
            .field("package", &self.package)
            .field("name", &self.name)
            .field("file", &self.file)
            .field("lines", &self.lines)
            .field("xsub", &self.xsub)
            .field("proto", &self.proto)
            .field("home", &self.home)
            .field("gv", &self.gv)
            .field("cv_addr", &format_args!("{:#x}", self.cv_addr))
            .finish()
    }
}

impl SubInfo {
    pub fn qualified_name(&self) -> String {
        format!("{}::{}", self.package, self.name)
    }
}

fn info_of(perl: &Perl, e: &libperl_rs::SubEntry) -> SubInfo {
    let cv = e.cv;
    let xsub = cv.is_xsub();
    let home = cv_home(cv).filter(|h| h.package != e.package || h.name != e.name);
    SubInfo {
        package: e.package.clone(),
        name: e.name.clone(),
        file: cv.file(),
        lines: if xsub { None } else { cv_lines(perl, cv) },
        xsub,
        proto: cv.proto(),
        home,
        gv: e
            .gv
            .as_ref()
            .and_then(|g| Some((g.file()?, g.line()?))),
        cv_addr: cv.as_ptr() as usize,
        cv,
    }
}

/// Every named sub reachable from `main::`, in symbol-table order.
pub fn list_subs(perl: &Perl) -> Vec<SubInfo> {
    let mut out = Vec::new();
    let mut walker = StashWalker::new(perl);
    walker.walk("main", &mut |e| out.push(info_of(perl, e)));
    out
}

/// The subs whose `CvFILE` is `file` (exact match).
pub fn subs_in_file(perl: &Perl, file: &str) -> Vec<SubInfo> {
    list_subs(perl)
        .into_iter()
        .filter(|s| s.file.as_deref() == Some(file))
        .collect()
}

/// Look a sub up by its package-qualified name (`"Foo::bar"`, or `"bar"`
/// in `main`), without creating anything in the symbol table.
pub fn find_cv(perl: &Perl, qualified: &str) -> Option<Cv> {
    let cname = std::ffi::CString::new(qualified).ok()?;
    let p = perl_call!(perl.as_ptr(), Perl_get_cv(cname.as_ptr(), 0));
    Cv::from_raw(p)
}

/// [`crate::capture_sub`] of the sub named `qualified`.
pub fn capture_sub_by_name(
    perl: &Perl,
    qualified: &str,
) -> Result<inspect_core::ir::SubIr, String> {
    let cv = find_cv(perl, qualified).ok_or_else(|| format!("no sub named {qualified}"))?;
    crate::capture_sub(perl, cv)
}
