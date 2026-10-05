//! Owned IR of the OP tree.
//!
//! Design notes:
//! - `id` is a pre-order sequence number. It underpins golden JSON stability
//!   and the id-based resolution of `next`/`other` references (2-pass capture).
//! - Nulled ops (OP_NULL) are kept in the tree, with the original op name
//!   recorded in `was`. Analysis passes look through them via `skip_null`
//!   (the same policy as B::Deparse).
//! - Pad names/types are not embedded in each node but centralized in
//!   `SubIr::pad` (looked up by targ).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubIr {
    /// CvFILE. "(eval N)" for subs that come from eval
    pub file: Option<String>,
    /// (min line, max line) over the COPs in the tree
    pub lines: Option<(u32, u32)>,
    /// CvPROTO (not yet supported: always None. Planned for M5)
    pub proto: Option<String>,
    /// Named pad entries (targ → name / declared type)
    pub pad: Vec<PadEntry>,
    /// Node id corresponding to CvSTART (start of execution order)
    pub start_id: Option<u32>,
    /// The tree under CvROOT
    pub root: OpNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PadEntry {
    pub ix: u32,
    pub name: Option<String>,
    /// Stash name of PadnameTYPE (the Foo in `my Foo $x`)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub typ: Option<String>,
    /// Raw value of xpadn_flags (see [`padname_flags`])
    pub flags: u8,
    /// For an `our` declaration, the package it belongs to
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub our_stash: Option<String>,
    /// Line of the declaring statement (None for captured outer names or
    /// when undeterminable)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub line: Option<u32>,
    /// First and last line of the statements that can see the name
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub scope: Option<(u32, u32)>,
}

/// Bits of [`PadEntry::flags`] (PADNAMEf_* in pad.h)
pub mod padname_flags {
    /// captured from an enclosing sub (closure)
    pub const OUTER: u8 = 0x01;
    pub const STATE: u8 = 0x02;
    /// `field` of a `class` (perl 5.38+)
    pub const FIELD: u8 = 0x20;
}

/// Result of Perl_op_class. The serde names match the return values of
/// B::class so the oracle tests (comparison against B) pass straight through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpClass {
    #[serde(rename = "NULL")]
    Null,
    #[serde(rename = "OP")]
    Base,
    #[serde(rename = "UNOP")]
    Unop,
    #[serde(rename = "BINOP")]
    Binop,
    #[serde(rename = "LOGOP")]
    Logop,
    #[serde(rename = "LISTOP")]
    Listop,
    #[serde(rename = "PMOP")]
    Pmop,
    #[serde(rename = "SVOP")]
    Svop,
    #[serde(rename = "PADOP")]
    Padop,
    #[serde(rename = "PVOP")]
    Pvop,
    #[serde(rename = "LOOP")]
    Loop,
    #[serde(rename = "COP")]
    Cop,
    #[serde(rename = "METHOP")]
    Methop,
    #[serde(rename = "UNOP_AUX")]
    UnopAux,
}

impl std::fmt::Display for OpClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            OpClass::Null => "NULL",
            OpClass::Base => "OP",
            OpClass::Unop => "UNOP",
            OpClass::Binop => "BINOP",
            OpClass::Logop => "LOGOP",
            OpClass::Listop => "LISTOP",
            OpClass::Pmop => "PMOP",
            OpClass::Svop => "SVOP",
            OpClass::Padop => "PADOP",
            OpClass::Pvop => "PVOP",
            OpClass::Loop => "LOOP",
            OpClass::Cop => "COP",
            OpClass::Methop => "METHOP",
            OpClass::UnopAux => "UNOP_AUX",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpNode {
    /// Pre-order sequence number (root = 0)
    pub id: u32,
    /// PL_op_name[op_type]
    pub name: String,
    pub op_type: u16,
    pub class: OpClass,
    /// For OP_NULL, the op name before it was nulled
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub was: Option<String>,
    pub flags: u8,
    pub private: u8,
    pub targ: u64,
    /// Node id of op_next (next in execution order)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub next: Option<u32>,
    /// Node id of a LOGOP's op_other (branch target)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub other: Option<u32>,
    #[serde(skip_serializing_if = "OpDetail::is_none", default)]
    pub detail: OpDetail,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub kids: Vec<OpNode>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub enum OpDetail {
    #[default]
    None,
    /// Value of an SVOP (const etc.). Under ithreads, pad-based cases are already resolved
    Const(SvLit),
    /// GV reference (a PADOP under ithreads). name is the GV name, stash the package name
    Gv {
        name: String,
        stash: Option<String>,
    },
    Cop {
        file: Option<String>,
        line: u32,
    },
    /// METHOP. name=None means a dynamic method call
    Method {
        name: Option<String>,
    },
    /// argcheck of a signature (decoded in M2)
    ArgCheck {
        params: u64,
        opt: u64,
        slurpy: Option<char>,
    },
    /// argelem of a signature
    ArgElem {
        index: u64,
    },
    /// multiparam of a signature (perl 5.44+, replaces argcheck/argelem).
    /// Pad indexes are 0 for a placeholder without a name (`$`, `@`).
    /// Defaults live in separate paramtest/paramstore ops (targ = padix).
    MultiParam {
        /// number of mandatory positional parameters
        min_args: u64,
        /// mandatory + optional positional parameters (excluding slurpy)
        n_positional: u64,
        slurpy: Option<char>,
        param_padix: Vec<u64>,
        slurpy_padix: u64,
        named: Vec<NamedParam>,
    },
    /// Branch-target node ids of the LOOP struct
    Loop {
        redo: Option<u32>,
        next: Option<u32>,
        last: Option<u32>,
    },
    /// PMOP. Retrieving the pattern is an M5 stretch goal
    Pm {
        pattern: Option<String>,
    },
    /// Decoded multideref (UNOP_AUX): the deref chain of e.g. `$x->[0]{k}`
    MultiDeref {
        steps: Vec<DerefStep>,
    },
    /// multiconcat (UNOP_AUX): the nargs+1 constant string segments between
    /// (and around) the concatenated operands, in order. `None` marks a
    /// position with no constant segment (aux length -1). `Aux("multiconcat")`
    /// remains the marker for an undecoded op.
    MultiConcat {
        pieces: Vec<Option<String>>,
    },
    /// Marker for undecoded auxiliary data (UNOP_AUX etc.)
    Aux(String),
}

/// A named signature parameter (`:$name`, perl 5.44+)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedParam {
    pub padix: u64,
    /// the key as passed by the caller (without sigil)
    pub name: String,
    pub required: bool,
}

/// One step of a multideref. base corresponds to the MDEREF action names in op.h:
/// padsv (a lexical holding a ref) / padav / padhv (an aggregate lexical directly) /
/// gvsv / gvav / gvhv (package variables) / chain (result of the previous step) /
/// stack (result of a preceding op)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerefStep {
    /// "array" | "hash"
    pub container: String,
    pub base: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base_targ: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base_name: Option<String>,
    /// Index/key (the value for const, the variable name for padsv, $name for gvsv; None if dynamic)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub key: Option<String>,
}

impl OpDetail {
    pub fn is_none(&self) -> bool {
        matches!(self, OpDetail::None)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SvLit {
    Undef,
    Iv(i64),
    Uv(u64),
    Nv(f64),
    Pv(String),
    RefTo(Box<SvLit>),
    Code,
    Glob { name: String, stash: Option<String> },
    Other(String),
}

impl SubIr {
    /// References to all nodes in pre-order (id order)
    pub fn nodes(&self) -> Vec<&OpNode> {
        let mut acc = Vec::new();
        collect(&self.root, &mut acc);
        acc
    }

    pub fn node_by_id(&self, id: u32) -> Option<&OpNode> {
        // id is a pre-order sequence number, so it matches the index into nodes()
        self.nodes().into_iter().nth(id as usize)
    }

    /// Look up a pad name by targ
    pub fn pad_name(&self, targ: u64) -> Option<&str> {
        self.pad
            .iter()
            .find(|e| e.ix as u64 == targ)
            .and_then(|e| e.name.as_deref())
    }

    /// Line of the statement that introduces each pad entry, keyed by pad
    /// index: the first op (pre-order) that declares it — a pad op with
    /// OPpLVAL_INTRO (`my $x`, `my ($a, $b) = @_` via padrange, `for my
    /// $i`), or a signature parameter (argelem / multiparam). `our`
    /// declarations have no such pad op and are not covered.
    pub fn intro_lines(&self) -> HashMap<u64, u32> {
        const OPP_LVAL_INTRO: u8 = 0x80;
        const OPP_PADRANGE_COUNTMASK: u8 = 0x7f;
        let mut out: HashMap<u64, u32> = HashMap::new();
        crate::passes::walk_with_lines(self, |node, line| {
            let Some(line) = line else { return };
            let mut intro = |ix: u64| {
                out.entry(ix).or_insert(line);
            };
            // live ops only: op_null() reuses op_targ for the original op
            // type, so a nulled op's targ is not a pad index
            let name = node.name.as_str();
            let lval_intro = node.private & OPP_LVAL_INTRO != 0;
            match (name, &node.detail) {
                ("padsv" | "padav" | "padhv" | "padsv_store" | "enteriter", _) if lval_intro => {
                    intro(node.targ)
                }
                ("padrange", _) if lval_intro => {
                    let n = (node.private & OPP_PADRANGE_COUNTMASK) as u64;
                    (node.targ..node.targ + n).for_each(&mut intro);
                }
                ("argelem", _) => intro(node.targ),
                (
                    _,
                    OpDetail::MultiParam {
                        param_padix,
                        slurpy_padix,
                        named,
                        ..
                    },
                ) => {
                    param_padix
                        .iter()
                        .chain(std::iter::once(slurpy_padix))
                        .chain(named.iter().map(|n| &n.padix))
                        .filter(|ix| **ix != 0)
                        .for_each(|ix| intro(*ix));
                }
                _ => {}
            }
        });
        out
    }
}

fn collect<'a>(node: &'a OpNode, acc: &mut Vec<&'a OpNode>) {
    acc.push(node);
    for k in &node.kids {
        collect(k, acc);
    }
}

impl OpNode {
    /// Look through OP_NULL to get the real node (B::Deparse's policy).
    /// Descends into the sole child of a nulled op if there is one.
    pub fn skip_null(&self) -> &OpNode {
        let mut cur = self;
        while cur.op_type == 0 && cur.kids.len() == 1 {
            cur = &cur.kids[0];
        }
        cur
    }
}
