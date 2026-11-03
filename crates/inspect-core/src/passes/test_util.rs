//! Hand-built IR for unit tests of the passes.

use crate::ir::{OpClass, OpDetail, OpNode, PadEntry, SubIr, SvLit};

pub(crate) fn node(id: u32, name: &str, kids: Vec<OpNode>) -> OpNode {
    OpNode {
        id,
        name: name.into(),
        op_type: 1,
        class: OpClass::Base,
        was: None,
        flags: 0,
        private: 0,
        targ: 0,
        next: None,
        other: None,
        detail: OpDetail::None,
        kids,
    }
}

pub(crate) fn pad(id: u32, targ: u64) -> OpNode {
    let mut n = node(id, "padsv", vec![]);
    n.targ = targ;
    n
}

pub(crate) fn konst(id: u32, iv: i64) -> OpNode {
    let mut n = node(id, "const", vec![]);
    n.detail = OpDetail::Const(SvLit::Iv(iv));
    n
}

/// A sub with pad entries `$x` (1) and `$y` (2) around `root`.
pub(crate) fn ir_with(root: OpNode) -> SubIr {
    let entry = |ix: u32, name: &str| PadEntry {
        ix,
        name: Some(name.into()),
        typ: None,
        flags: 0,
        our_stash: None,
        line: None,
        scope: None,
    };
    SubIr {
        file: None,
        package: None,
        lines: None,
        proto: None,
        pad: vec![entry(1, "$x"), entry(2, "$y")],
        start_id: None,
        root,
        anon_subs: vec![],
    }
}

/// A SubIr from a golden JSON file (`include_str!` of t/golden/...).
pub(crate) fn ir_of(golden: &str) -> SubIr {
    serde_json::from_str(golden).expect("golden fixture deserializes")
}
