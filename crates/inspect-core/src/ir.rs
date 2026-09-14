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
    /// Raw value of xpadn_flags
    pub flags: u8,
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
    /// Marker for undecoded auxiliary data (UNOP_AUX etc.)
    Aux(String),
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
    Glob {
        name: String,
        stash: Option<String>,
    },
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
