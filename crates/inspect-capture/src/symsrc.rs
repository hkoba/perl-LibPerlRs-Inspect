//! [`IrSource`] backed by the running interpreter: supplies the bodies of
//! argument helpers for inlining by `inspect_core::passes::symexec`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use inspect_core::ir::{SubIr, SvLit};
use inspect_core::passes::symexec::{HashSeg, IrSource, Sym};
use libperl_rs::Perl;

use crate::packages::cv_home;
use crate::subs::find_cv;

/// Supplies the subs named in a helper list, and every sub defined in
/// the same (home) package as one of them, captured on first use.
pub struct PerlSource<'a> {
    perl: &'a Perl,
    /// Helper names resolved to where they are defined.
    helpers: Vec<String>,
    /// Home packages of the helpers.
    packages: Vec<String>,
    cache: RefCell<HashMap<String, Option<Arc<SubIr>>>>,
}

fn qualify(name: &str, pkg: &str) -> String {
    if name.contains("::") {
        name.to_string()
    } else {
        format!("{pkg}::{name}")
    }
}

/// Where the sub `qualified` is defined (`Home`), as a qualified name.
fn home_name(perl: &Perl, qualified: &str) -> Option<String> {
    let cv = find_cv(perl, qualified)?;
    let home = cv_home(cv)?;
    (!home.anon).then(|| format!("{}::{}", home.package, home.name))
}

impl<'a> PerlSource<'a> {
    /// `helpers` are qualified sub names (an imported name works too: it is
    /// resolved to where the sub is defined).
    pub fn new(perl: &'a Perl, helpers: &[String]) -> PerlSource<'a> {
        let helpers: Vec<String> = helpers
            .iter()
            .map(|h| qualify(h, "main"))
            .map(|h| home_name(perl, &h).unwrap_or(h))
            .collect();
        let mut packages: Vec<String> = helpers
            .iter()
            .filter_map(|h| h.rsplit_once("::").map(|(p, _)| p.to_string()))
            .collect();
        packages.sort();
        packages.dedup();
        PerlSource {
            perl,
            helpers,
            packages,
            cache: RefCell::new(HashMap::new()),
        }
    }

    fn wanted(&self, qualified: &str) -> bool {
        self.helpers.iter().any(|h| h == qualified)
            || qualified
                .rsplit_once("::")
                .is_some_and(|(p, _)| self.packages.iter().any(|x| x == p))
    }
}

impl IrSource for PerlSource<'_> {
    fn sub_ir(&self, callee: &str, caller_pkg: &str) -> Option<Arc<SubIr>> {
        let q = qualify(callee, caller_pkg);
        let q = home_name(self.perl, &q).unwrap_or(q);
        if !self.wanted(&q) {
            return None;
        }
        if let Some(hit) = self.cache.borrow().get(&q) {
            return hit.clone();
        }
        let ir = find_cv(self.perl, &q)
            .and_then(|cv| crate::capture_sub(self.perl, cv).ok())
            .map(Arc::new);
        self.cache.borrow_mut().insert(q, ir.clone());
        ir
    }

    fn canonical_name(&self, callee: &str, caller_pkg: &str) -> Option<String> {
        home_name(self.perl, &qualify(callee, caller_pkg))
    }

    fn outer_value(&self, callee: &str, pad_ix: u32) -> Option<Sym> {
        let ir = self.sub_ir(callee, "main")?;
        let sigil = ir
            .pad
            .iter()
            .find(|e| e.ix == pad_ix)?
            .name
            .as_deref()?
            .chars()
            .next()?;
        let cv = find_cv(self.perl, callee)?;
        let sv = crate::raw::PAD_BASE_SV(cv.padlist(), pad_ix as isize);
        if sv.is_null() {
            return None;
        }
        match sigil {
            '$' => scalar_sym(sv),
            '@' => {
                let av = unsafe { libperl_rs::Av::from_raw_unchecked(sv as *mut libperl_sys::AV) };
                let items: Option<Vec<Sym>> = av
                    .iter(self.perl)
                    .map(|e| e.map_or(Some(Sym::undef()), |e| scalar_sym(e.as_ptr())))
                    .collect();
                Some(Sym::List {
                    items: items?,
                    tail: None,
                })
            }
            '%' => {
                let hv = unsafe { libperl_rs::Hv::from_raw_unchecked(sv as *mut libperl_sys::HV) };
                let pairs: Option<Vec<(String, Sym)>> = hv
                    .iter(self.perl)
                    .map(|(k, v)| Some((String::from_utf8_lossy(k).into_owned(), scalar_sym(v.as_ptr())?)))
                    .collect();
                Some(Sym::Hash {
                    segs: vec![HashSeg::Pairs(pairs?)],
                })
            }
            _ => None,
        }
    }
}

/// A plain scalar as a constant (references and globs are not followed).
fn scalar_sym(sv: *const libperl_sys::SV) -> Option<Sym> {
    match crate::capture::sv_lit(sv) {
        SvLit::RefTo(_) | SvLit::Code | SvLit::Other(_) | SvLit::Glob { .. } => None,
        lit => Some(Sym::Const(lit)),
    }
}
