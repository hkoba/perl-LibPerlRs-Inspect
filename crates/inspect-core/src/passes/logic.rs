//! M3: ガード条件の構造的パス解析 (真理値表) v0。
//!
//! sub 本体を文単位に走査し、and / or / dor / cond_expr の分岐ごとに
//! パス条件 (原子条件の真偽の連言) を積み上げ、各到達点 (return / die /
//! croak / 暗黙の最終式) を「どの条件の組み合わせで到達するか」と共に
//! 記録する。原子条件が少数 (MAX_TABLE_CONDS 以下) なら全組み合わせの
//! 真理値表も生成する。
//!
//! v0 の割り切り (将来課題):
//! - 原子条件は描画テキストで同一視する (意味的含意、例えば
//!   `$v > 10` ⇒ `$v > 5` は考慮しない。真理値表には意味的に到達不能な
//!   行も含まれ得る)。
//! - 分岐の合流後は、両腕とも生き残る場合その分岐のリテラルを落とす
//!   (early-return 形: 片腕が必ず return/die で終わる場合のみ、
//!   継続側に否定リテラルを引き継ぐ)。
//! - 式の内部 (代入の右辺など) に埋まった三項演算子等は原子に拾わない。
//! - eval BLOCK 内の die は捕捉されるが、kind は "die" のまま報告する。
//! - ループはループ制御条件を原子に含めず、本体の分岐だけを拾う。

use serde::{Deserialize, Serialize};

use crate::ir::{OpClass, OpDetail, OpNode, SubIr};
use crate::render::render;

use super::retspec::{croak_call, return_exprs};

/// これを超える原子数では真理値表 (2^n 行) を生成しない
pub const MAX_TABLE_CONDS: usize = 6;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogicSpec {
    /// 原子条件の描画テキスト (初出順)。`when` / `table` の添字が指す
    pub conds: Vec<String>,
    /// 到達点とそのパス条件 (ソース順)
    pub paths: Vec<PathOutcome>,
    /// 真理値表。conds が空か MAX_TABLE_CONDS 超なら None
    pub table: Option<Vec<TableRow>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathOutcome {
    pub line: Option<u32>,
    /// return | die | croak | confess | implicit
    pub kind: String,
    pub exprs: Vec<String>,
    /// このパスに到達する条件 (リテラルの連言)
    pub when: Vec<CondLit>,
}

/// パス条件のリテラル: conds[cond] が value であること
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CondLit {
    pub cond: usize,
    pub value: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableRow {
    /// conds と同順の真偽割り当て (conds[0] が最上位ビット順で列挙)
    pub inputs: Vec<bool>,
    /// paths のうち最初に when が成立するものの添字 (該当なしは None)
    pub path: Option<usize>,
}

pub fn analyze_logic(ir: &SubIr) -> LogicSpec {
    let mut w = Walker {
        ir,
        conds: Vec::new(),
        paths: Vec::new(),
    };
    let stmts = super::statements(ir);
    w.walk_block(&stmts, &[], true);
    let table = w.build_table();
    LogicSpec {
        conds: w.conds,
        paths: w.paths,
        table,
    }
}

/// 文の処理結果: 後続の文がどう続くか
enum Flow {
    /// 普通に続く
    Continue,
    /// この文の片腕が必ず終端したので、継続側はリテラル付きで続く
    ContinueWith(Vec<CondLit>),
    /// 全パスが return/die で終端した (以降の文は到達不能)
    Terminates,
}

struct Walker<'a> {
    ir: &'a SubIr,
    conds: Vec<String>,
    paths: Vec<PathOutcome>,
}

impl<'a> Walker<'a> {
    /// 文リストを順に処理する。戻り値は「全パスが終端したか」。
    /// `tail` はこのリストが sub (または腕) の末尾位置にあるか。
    fn walk_block(&mut self, stmts: &[(Option<u32>, &'a OpNode)], path: &[CondLit], tail: bool) -> bool {
        let mut path = path.to_vec();
        for (i, (line, node)) in stmts.iter().enumerate() {
            let stmt_tail = tail && i + 1 == stmts.len();
            match self.walk_stmt(*line, node, &path, stmt_tail) {
                Flow::Terminates => return true,
                Flow::ContinueWith(extra) => path.extend(extra),
                Flow::Continue => {}
            }
        }
        false
    }

    fn walk_stmt(&mut self, line: Option<u32>, node: &'a OpNode, path: &[CondLit], tail: bool) -> Flow {
        let n = node.skip_null();
        match n.name.as_str() {
            "return" => {
                let exprs = return_exprs(self.ir, n);
                self.emit(line, "return", exprs, path);
                Flow::Terminates
            }
            "die" => {
                let exprs = return_exprs(self.ir, n);
                self.emit(line, "die", exprs, path);
                Flow::Terminates
            }
            "entersub" => {
                if let Some((via, msg)) = croak_call(self.ir, n) {
                    self.emit(line, &via, msg.into_iter().collect(), path);
                    Flow::Terminates
                } else {
                    self.tail_implicit(line, node, path, tail);
                    Flow::Continue
                }
            }
            "cond_expr" => self.walk_cond_expr(line, n, path, tail),
            "and" | "or" | "dor" => self.walk_logop(line, n, path, tail),
            "scope" | "leave" | "lineseq" => {
                let stmts = stmt_list(n, line);
                if self.walk_block(&stmts, path, tail) {
                    Flow::Terminates
                } else {
                    Flow::Continue
                }
            }
            "leavetry" => {
                // eval BLOCK: 中の die は捕捉されるので、外は常に続行
                let stmts = stmt_list(n, line);
                self.walk_block(&stmts, path, false);
                Flow::Continue
            }
            "leaveloop" => {
                self.walk_loop(line, n, path);
                Flow::Continue
            }
            _ => {
                self.tail_implicit(line, node, path, tail);
                Flow::Continue
            }
        }
    }

    /// if/elsif/else・三項演算子 (LOGOP cond_expr: [条件, 真腕, 偽腕])
    fn walk_cond_expr(&mut self, line: Option<u32>, n: &'a OpNode, path: &[CondLit], tail: bool) -> Flow {
        let Some(cond) = n.kids.first() else {
            return Flow::Continue;
        };
        let (idx, pol) = self.intern_cond(cond);
        let mut t_path = path.to_vec();
        t_path.push(CondLit { cond: idx, value: pol });
        let t_term = self.walk_arm(n.kids.get(1), line, t_path, tail);
        let mut f_path = path.to_vec();
        f_path.push(CondLit { cond: idx, value: !pol });
        let f_term = self.walk_arm(n.kids.get(2), line, f_path, tail);
        match (t_term, f_term) {
            (true, true) => Flow::Terminates,
            (true, false) => Flow::ContinueWith(vec![CondLit { cond: idx, value: !pol }]),
            (false, true) => Flow::ContinueWith(vec![CondLit { cond: idx, value: pol }]),
            (false, false) => Flow::Continue,
        }
    }

    /// 文レベルの and / or / dor (`STMT if COND` / `unless` / `//`)
    fn walk_logop(&mut self, line: Option<u32>, n: &'a OpNode, path: &[CondLit], tail: bool) -> Flow {
        let (Some(cond), Some(arm)) = (n.kids.first(), n.kids.get(1)) else {
            return Flow::Continue;
        };
        // arm_when: 右腕が走るときの原子の値
        let (idx, arm_when) = match n.name.as_str() {
            "and" => self.intern_cond(cond),
            "or" => {
                let (i, pol) = self.intern_cond(cond);
                (i, !pol)
            }
            // dor: 右腕は左辺が undef のときだけ走る
            _ => (
                self.intern_text(format!("defined({})", render(self.ir, cond))),
                false,
            ),
        };
        let mut a_path = path.to_vec();
        a_path.push(CondLit { cond: idx, value: arm_when });
        let arm_term = self.walk_arm(Some(arm), line, a_path, tail);
        if tail {
            // 逆側では logop 自身の値 (= 左辺の値) が式の値になる
            let mut f_path = path.to_vec();
            f_path.push(CondLit { cond: idx, value: !arm_when });
            self.emit(line, "implicit", vec![render(self.ir, cond)], &f_path);
        }
        if arm_term {
            Flow::ContinueWith(vec![CondLit { cond: idx, value: !arm_when }])
        } else {
            Flow::Continue
        }
    }

    /// 分岐の腕: ブロックなら文リストとして、単一式なら 1 文として辿る。
    /// 戻り値は「腕の全パスが終端したか」。
    fn walk_arm(&mut self, node: Option<&'a OpNode>, line: Option<u32>, path: Vec<CondLit>, tail: bool) -> bool {
        let Some(node) = node else {
            return false;
        };
        let n = node.skip_null();
        match n.name.as_str() {
            "scope" | "leave" | "lineseq" => {
                let stmts = stmt_list(n, line);
                self.walk_block(&stmts, &path, tail)
            }
            _ => self.walk_block(&[(line, node)], &path, tail),
        }
    }

    /// leaveloop: ループ制御 (iter / 継続条件) は原子に含めず、
    /// 本体の分岐だけを拾う。本体は 0 回実行もあり得るので終端扱いしない。
    fn walk_loop(&mut self, line: Option<u32>, n: &'a OpNode, path: &[CondLit]) {
        for k in &n.kids {
            let s = k.skip_null();
            match s.name.as_str() {
                "enteriter" | "enterloop" => {}
                "and" | "or" => {
                    if let Some(body) = s.kids.get(1) {
                        self.walk_arm(Some(body), line, path.to_vec(), false);
                    }
                }
                _ => {
                    self.walk_stmt(line, k, path, false);
                }
            }
        }
    }

    /// 末尾位置の式文なら暗黙の戻り値として記録する
    fn tail_implicit(&mut self, line: Option<u32>, node: &'a OpNode, path: &[CondLit], tail: bool) {
        if tail {
            self.emit(line, "implicit", vec![render(self.ir, node)], path);
        }
    }

    fn emit(&mut self, line: Option<u32>, kind: &str, exprs: Vec<String>, when: &[CondLit]) {
        self.paths.push(PathOutcome {
            line,
            kind: kind.to_string(),
            exprs,
            when: when.to_vec(),
        });
    }

    /// 条件ノードを原子として登録し (描画テキストで同一視)、
    /// (添字, 真腕に対応する原子の値) を返す。`not` は原子を裏返す。
    /// elsif の条件は lineseq(ex-nextstate, 条件) に包まれるので剥がす。
    fn intern_cond(&mut self, node: &'a OpNode) -> (usize, bool) {
        let mut n = node.skip_null();
        if n.name == "lineseq" {
            if let Some(last) = n.kids.iter().filter(|k| k.class != OpClass::Cop).last() {
                n = last.skip_null();
            }
        }
        if n.name == "not" {
            if let Some(inner) = n.kids.first() {
                return (self.intern_text(render(self.ir, inner)), false);
            }
        }
        (self.intern_text(render(self.ir, n)), true)
    }

    fn intern_text(&mut self, text: String) -> usize {
        if let Some(i) = self.conds.iter().position(|c| *c == text) {
            i
        } else {
            self.conds.push(text);
            self.conds.len() - 1
        }
    }

    /// 全組み合わせを列挙し、各行で最初に when が成立するパスを引く
    fn build_table(&self) -> Option<Vec<TableRow>> {
        let n = self.conds.len();
        if n == 0 || n > MAX_TABLE_CONDS {
            return None;
        }
        let mut rows = Vec::with_capacity(1 << n);
        for mask in 0..(1u32 << n) {
            let inputs: Vec<bool> = (0..n).map(|i| mask >> (n - 1 - i) & 1 == 1).collect();
            let path = self
                .paths
                .iter()
                .position(|p| p.when.iter().all(|l| inputs[l.cond] == l.value));
            rows.push(TableRow { inputs, path });
        }
        Some(rows)
    }
}

/// ブロック op の子を COP で区切って (行番号, 文) の列にする。
/// enter / entertry / unstack は文として数えない。
fn stmt_list<'a>(n: &'a OpNode, inherited: Option<u32>) -> Vec<(Option<u32>, &'a OpNode)> {
    let mut line = inherited;
    let mut out = Vec::new();
    for k in &n.kids {
        if k.class == OpClass::Cop {
            if let OpDetail::Cop { line: l, .. } = &k.detail {
                line = Some(*l);
            }
            continue;
        }
        match k.name.as_str() {
            "enter" | "entertry" | "unstack" => {}
            _ => out.push((line, k)),
        }
    }
    out
}
