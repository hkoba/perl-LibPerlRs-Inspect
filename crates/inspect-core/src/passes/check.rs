//! Check a call site against the argument spec of its callee. Only what
//! is certain is reported: a count outside the spec's bounds, a constant
//! key the callee never reads, a required key that is not passed.

use serde::{Deserialize, Serialize};

use super::argspec::{ArgSpec, Norm, Slot};
use super::callsites::CallSite;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// too_few | too_many | unknown_key | missing_key | note
    pub kind: String,
    pub message: String,
}

fn finding(kind: &str, message: String) -> Finding {
    Finding {
        kind: kind.into(),
        message,
    }
}

fn normalize(key: &str, norm: &[Norm]) -> String {
    let mut k = key.to_string();
    for n in norm {
        k = match n {
            Norm::Lc => k.to_lowercase(),
            Norm::StripPrefix(p) => k.strip_prefix(p.as_str()).map_or(k.clone(), String::from),
        };
    }
    k
}

/// Findings for `call` against the callee's `spec`. `callee_is_method`:
/// the call passes an invocant that is not among `call.args` (always the
/// case for a method call).
pub fn check_call(spec: &ArgSpec, call: &CallSite, callee_is_method: bool) -> Vec<Finding> {
    let mut out = Vec::new();
    let inv = u64::from(callee_is_method || call.kind == "method");
    if call.forwards_args {
        out.push(finding(
            "note",
            "the call passes the caller's @_ on; its argument count is not checked".into(),
        ));
    } else {
        if let Some(max) = call.argc.max {
            if max + inv < spec.min_arity {
                out.push(finding(
                    "too_few",
                    format!(
                        "{} argument(s) passed, at least {} expected",
                        max + inv,
                        spec.min_arity
                    ),
                ));
            }
        }
        if let Some(max) = spec.max_arity {
            if call.argc.min + inv > max {
                out.push(finding(
                    "too_many",
                    format!(
                        "{} argument(s) passed, at most {} expected",
                        call.argc.min + inv,
                        max
                    ),
                ));
            }
        }
    }

    // keys: only when the pairs start exactly where the callee's hash of
    // options takes the arguments (`my ($self, %args) = @_`), and the
    // callee does not pass its arguments on to someone else
    let Some(pairs) = &call.pairs else {
        return out;
    };
    let start = call.pairs_from + inv;
    let hash_at_start = spec.params.iter().any(|p| {
        p.name.as_deref().is_some_and(|n| n.starts_with('%')) && p.alts == [Slot::Rest(start)]
    });
    if !hash_at_start || !spec.forwards.is_empty() || spec.named.is_empty() {
        return out;
    }
    let known = |k: &str| {
        spec.named.iter().any(|n| {
            let nk = normalize(k, &n.normalize);
            nk == n.key || n.aliases.contains(&nk)
        })
    };
    for (k, _) in pairs {
        if !known(k) {
            out.push(finding("unknown_key", format!("key '{}' is not read by the callee", k)));
        }
    }
    for n in spec.named.iter().filter(|n| n.required) {
        let passed = pairs.iter().any(|(k, _)| {
            let nk = normalize(k, &n.normalize);
            nk == n.key || n.aliases.contains(&nk)
        });
        if !passed {
            out.push(finding("missing_key", format!("required key '{}' is not passed", n.key)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::passes::argspec::analyze_args;
    use crate::passes::callsites::{Arity, Callee, CallSite};
    use crate::passes::test_util::ir_of;

    fn call(argc: Arity, pairs: Option<Vec<&str>>, kind: &str) -> CallSite {
        CallSite {
            node: 0,
            line: None,
            callee: Callee::Named { name: "main::f".into() },
            args: Vec::new(),
            argc,
            pairs: pairs.map(|p| p.into_iter().map(|k| (k.to_string(), None)).collect()),
            pairs_from: 0,
            forwards_args: false,
            kind: kind.into(),
            line_seq: 0,
        }
    }

    fn kinds(f: &[Finding]) -> Vec<&str> {
        f.iter().map(|f| f.kind.as_str()).collect()
    }

    #[test]
    fn arity_against_checks() {
        // croak unless @_ == 2; die if @_ > 5; my ($x, $y) = @_
        let spec = analyze_args(&ir_of(include_str!(
            "../../../../t/golden/5.42-threaded/args_checks.json"
        )));
        let f = |min, max| check_call(&spec, &call(Arity { min, max }, None, "call"), false);
        assert_eq!(kinds(&f(1, Some(1))), ["too_few"]);
        assert_eq!(kinds(&f(3, Some(3))), ["too_many"]);
        assert!(f(2, Some(2)).is_empty());
        assert!(f(0, None).is_empty(), "an open list may hold enough");
    }

    #[test]
    fn keys_of_a_method_with_options() {
        // my ($self, %args) = @_; $args{foo} // ...; $args{bar}; exists $args{baz}
        let spec = analyze_args(&ir_of(include_str!(
            "../../../../t/golden/5.42-threaded/args_opts_hash.json"
        )));
        let m = |keys: Vec<&str>| {
            let n = keys.len() as u64 * 2;
            check_call(&spec, &call(Arity { min: n, max: Some(n) }, Some(keys), "method"), false)
        };
        assert!(m(vec!["foo", "bar"]).is_empty());
        assert_eq!(kinds(&m(vec!["bar", "qux"])), ["unknown_key"]);
        assert_eq!(kinds(&m(vec!["foo"])), ["missing_key"]);
        // as a plain function call the pairs start at $self's slot
        let plain = check_call(
            &spec,
            &call(Arity { min: 2, max: Some(2) }, Some(vec!["qux"]), "call"),
            false,
        );
        assert!(plain.is_empty());
    }
}
