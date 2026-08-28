# OpTree::Analyzer → LibPerlRs::Inspect 改名 (2026-08-28)

libperl-rs の次期プロジェクト提案
(`libperl-rs/docs/plan/roadmap-next-projects-2026-08.md`) の決定に従い、
本リポジトリを libperl-rs ファミリーの第 1 製品 **LibPerlRs::Inspect**
(Semantic Inspector) の土台として位置づけ、家名憲章
(perl-LibPerlRs-PartialEval `docs/DESIGN.md` §8: Perl 名前空間は
`LibPerlRs::*`) に合わせて改名した。旧名 `OpTree::Analyzer` は
status doc (docs/status-2026-07-05.md) 自身が「仮称」と明記していたもの。

## 変更内容 (機械的改名のみ、挙動変更なし)

| 対象 | 旧 | 新 |
|---|---|---|
| Perl モジュール | `OpTree::Analyzer` | `LibPerlRs::Inspect` |
| dist 名 | OpTree-Analyzer | LibPerlRs-Inspect |
| .so 配置 | auto/OpTree/Analyzer/Analyzer.so | auto/LibPerlRs/Inspect/Inspect.so |
| crates | analyzer-core / analyzer-capture / analyzer-xs | inspect-core / inspect-capture / inspect-xs |
| cdylib | libanalyzer_xs.so | libinspect_xs.so |

`$VERSION` (0.01)、API (`analyze` / `analyze_json` / `op_names` /
`capture` / `dump_optree`)、テスト (9 ファイル 72 件)、golden fixture は
すべて不変。改名後 `perl Makefile.PL && make debug && prove -b t/` で
全 PASS を確認済み (perl 5.42.3 threaded)。

docs/ 配下の既存文書と plan.md は歴史的記録として旧名のまま残す。

## 次のステップ (roadmap §3.2 の Inspect マイルストーン)

1. ~~libperl-rs の Step 2 introspection 層 (PR #24) 公開後、
   inspect-capture/raw.rs を新 API (Op/Cop/Gv/PadName/StashWalker) 消費に
   切り替えて薄化する。~~ **完了 (2026-08-28)** — PR #24 merge 後に
   [patch.crates-io] でローカル参照して実施し、同日 **libperl-rs 0.4.4**
   として crates.io 公開されたため patch を撤去して dep を 0.4.4 に bump
   済み (Step 2 層は 0.5 でなく 0.4.4 でリリースされた)。
   golden 72 テスト同一 = 挙動不変。
2. ~~`inspect-cli` crate (自前インタプリタ内蔵 `perl-inspect`) の新設~~
   **完了 (2026-08-28)** — MVP: compile-not-run + stash walk + 由来タグ
   (file/imported/xs) + 行範囲 + argspec (`--deep` で全レポート) +
   `schema_version: 1` JSON。end-to-end テスト 2 件付き。
   残: Makefile.PL (EXE_FILES 相当) での staging/install 統合。
3. リポジトリ名/公開 (GitHub `perl-LibPerlRs-Inspect` 想定) はユーザー判断。
4. 次の増分候補: M2 = compile.rs 系 BEGIN 帰属のファイル compile 移植で
   import 引数付き `use` 一覧 (`compile_info`) を JSON に載せる
   (roadmap §3.2 M2、採用フック)。PAD_BASE_SV の libperl-rs upstream。
