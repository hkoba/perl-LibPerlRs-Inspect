//! OP ツリーの所有型 IR。
//!
//! 設計メモ:
//! - `id` は pre-order 連番。golden JSON の安定性と、`next`/`other` の
//!   id 参照解決 (2 パスキャプチャ) の基盤。
//! - nulled op (OP_NULL) もツリーに保持し、元の op 名を `was` に記録する。
//!   解析パスは `skip_null` を通して実体を見る (B::Deparse と同じ方針)。
//! - pad 名/型は各ノードに埋め込まず `SubIr::pad` に一元化 (targ で引く)。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubIr {
    /// CvFILE。eval 由来の sub では "(eval N)"
    pub file: Option<String>,
    /// ツリー中の COP の (最小行, 最大行)
    pub lines: Option<(u32, u32)>,
    /// CvPROTO (未対応: 常に None。M5 で対応予定)
    pub proto: Option<String>,
    /// 名前付き pad エントリ (targ → 名前/宣言型)
    pub pad: Vec<PadEntry>,
    /// CvSTART に対応するノード id (実行順の起点)
    pub start_id: Option<u32>,
    /// CvROOT 以下のツリー
    pub root: OpNode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PadEntry {
    pub ix: u32,
    pub name: Option<String>,
    /// PadnameTYPE の stash 名 (`my Foo $x` の Foo)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub typ: Option<String>,
    /// xpadn_flags の生値
    pub flags: u8,
}

/// Perl_op_class の結果。serde 名は B::class の戻り値に一致させ、
/// oracle テスト (B との照合) を素通しにする。
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
    /// pre-order 連番 (root = 0)
    pub id: u32,
    /// PL_op_name[op_type]
    pub name: String,
    pub op_type: u16,
    pub class: OpClass,
    /// OP_NULL のとき、null 化される前の op 名
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub was: Option<String>,
    pub flags: u8,
    pub private: u8,
    pub targ: u64,
    /// op_next のノード id (実行順の次)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub next: Option<u32>,
    /// LOGOP の op_other (分岐先) のノード id
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
    /// SVOP (const 等) の値。ithreads では pad 経由の場合も解決済み
    Const(SvLit),
    /// GV 参照 (ithreads では PADOP)。name は GV 名、stash はパッケージ名
    Gv {
        name: String,
        stash: Option<String>,
    },
    Cop {
        file: Option<String>,
        line: u32,
    },
    /// METHOP。name=None は動的メソッド呼び出し
    Method {
        name: Option<String>,
    },
    /// signature の argcheck (M2 で decode)
    ArgCheck {
        params: u64,
        opt: u64,
        slurpy: Option<char>,
    },
    /// signature の argelem
    ArgElem {
        index: u64,
    },
    /// LOOP 構造体の分岐先ノード id
    Loop {
        redo: Option<u32>,
        next: Option<u32>,
        last: Option<u32>,
    },
    /// PMOP。pattern の取得は M5 stretch
    Pm {
        pattern: Option<String>,
    },
    /// 未デコードの補助データ (UNOP_AUX 等) の目印
    Aux(String),
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
    /// pre-order の全ノード参照 (id 順)
    pub fn nodes(&self) -> Vec<&OpNode> {
        let mut acc = Vec::new();
        collect(&self.root, &mut acc);
        acc
    }

    pub fn node_by_id(&self, id: u32) -> Option<&OpNode> {
        // id は pre-order 連番なので nodes() の添字と一致する
        self.nodes().into_iter().nth(id as usize)
    }

    /// targ から pad 名を引く
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
    /// OP_NULL を透過して実体のノードを得る (B::Deparse の方針)。
    /// null 化された op で唯一の子があればそちらへ降りる。
    pub fn skip_null(&self) -> &OpNode {
        let mut cur = self;
        while cur.op_type == 0 && cur.kids.len() == 1 {
            cur = &cur.kids[0];
        }
        cur
    }
}
