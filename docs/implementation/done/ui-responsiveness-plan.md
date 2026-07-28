# UI 応答性 — 評価・再構築回数の削減計画

> **Status**: Done (RESP-1 #191, RESP-2 #192, RESP-3 #193) — 2026-07-28

対象は `issues/README.md`「UI / 描画のもっさり」の**第1段**、すなわち
[CRIT-01](../../../issues/critical/CRIT-01-eval-update-notifies-whole-workspace.md) /
[HIGH-07](../../../issues/high/HIGH-07-document-changed-cascade-per-mouse-move.md) /
[HIGH-06](../../../issues/high/HIGH-06-pipeline-recompiled-per-param-edit.md) の3件。

第2段以降（描画1回あたりのコスト、評価器のアルゴリズム的コスト、メディア・
スクラブ）は `issues/` 側に残したまま、本計画では扱わない。
評価パス自体の計測記録は `../perf-baseline.md`、その前身計画は
`eval-render-performance-plan.md`。

## 背景

体感の遅さは「1回あたりのコスト」ではなく「**評価・再構築が走る回数**」に
支配されている。現状の通知経路は3箇所で回数を増幅している。

### 1. 評価結果の到着がワークスペース全体を再構築する

`ProjectState::on_eval_update` は `ViewerFrame` グローバルを publish した後、
さらに `cx.notify()` を呼ぶ（`crates/ravel-app/src/project_state.rs:960`）。
`ProjectState` の observer は6つある。

| observer | 位置 | notify で走る処理 |
|---|---|---|
| Timeline | `panels/timeline.rs:280` | `Composition` の deep compare → 無条件 `cx.notify()` (`:400`) |
| NodeEditor | `panels/node_editor.rs:504` | `Document` clone + `Graph` deep compare → 無条件 `cx.notify()` (`:745`) |
| Outliner | `panels/outliner.rs:97` | 全コンポジション・全レイヤー・全ノード走査で行を再構築 (`:146`) |
| MediaBin | `panels/media_bin.rs:78` | 行を再構築 (`:78`) |
| Properties | `panels/properties.rs:580` | 現ターゲットの値を全再解決 (`:584`) |
| Workspace（タイトル） | `workspace.rs:408` | 派生タイトルが変わった時だけ動く（既にガード済み） |

Viewer は `ProjectState` を観測していない。`ViewerFrame` グローバルを直接購読
している（`panels/viewer.rs:275`）ので、この notify は Viewer の更新に不要。
再生中は毎フレーム（30〜60Hz）評価結果が到着するため、ドキュメントが一切
変わっていないのに上記5パネルが毎フレーム再構築される。

`on_eval_update` が render から見える形で触る状態は次の3つだけ。

- `ViewerFrame` グローバル → Viewer が購読済み
- `NodeEvalTimings` グローバル → **NodeEditor が購読せずに render で読んでいる**
  （`panels/node_editor.rs:1638`、ノード下の負荷表示）。notify を外すと
  ここだけ更新が止まるので、明示購読へ移す必要がある
- `published_generation`（内部状態、render から不可視）

### 2. マウス移動ごとに document_changed の全カスケードが走る

`document_changed`（`project_state.rs:762`）はコンパイル済みチェーン破棄、
レイヤー / メディア選択のプルーン、`audio::sync_from_document`、評価要求、
そして `cx.notify()` を毎回実行する。スクラブティックとキャンバスドラッグは
move ごとに `apply_document` を呼ぶので、この全カスケードが入力レイテンシに
直接乗る。

さらに NodeEditor の `refresh_from_document` はグラフが変わると
`set_selected_nodes` を呼ぶ（`panels/node_editor.rs:729`）。ドラッグ中はグラフが
毎 move 変わるため、**選択集合が同一でも** `CanvasSelection` グローバルが
毎回 publish され、第2波の observer（Viewer の `selection_sub`
(`panels/viewer.rs:226`) が `document_has_node` 走査、Outliner が notify）が起動する。

なお `ProjectState` には既に `revision` があるが、これは
「非同期ロードの適用可否判定」用で、ロード適用自体は意図的に bump しない
（`project_state.rs:151-156`）。パネル側のゲートに流用すると
File ▸ Open の後にパネルが再構築されなくなるため、**別のカウンタが必要**。

### 3. パラメータ編集ごとに GPU パイプラインを再コンパイルする

`GpuEvalHooks::sync` は `InvalidationHint::Params` で編集ノードごとに
`processor_for_node` を呼び直す（`crates/ravel-app/src/eval_hooks.rs:73-84`）。
GPU ノードのコンストラクタは `ShaderManager::compile_source` →
`ComputePipeline::new` を通るため、変更イベントごとに

- `validate_wgsl`（naga の完全パース + 検証）が**ソースハッシュキャッシュ参照より
  前**に走る（`crates/ravel-gpu/src/shader.rs:133-139`）。モジュールキャッシュが
  検証コストを一切削っていない
- BindGroupLayout / PipelineLayout / ComputePipeline を新規作成
  （`crates/ravel-gpu/src/compute.rs:52-88`、ドライバ側コンパイル）

が走る。ところが GPU プロセッサのコンストラクタは5つすべて `_node: &Node` を
**受け取って使っていない**（`blur.rs:36`, `color_correct.rs:38`,
`transform.rs:42`, `merge.rs:46`, `rasterize/mod.rs:109`）。
ノード状態を一切キャプチャしていないので、パラメータ編集での再構築は完全な無駄。

なお issue が引く `ravel-gpu/src/lib.rs:86-89` の設計コメントは現在のソースに存在しない。
`InvalidationHint::Params` の doc は逆に「該当ノードのプロセッサだけ再構築する」と
書いており実装と一致している。矛盾は設計意図ではなく、GPU ノードではその再構築が
何も変えないという事実の側にある。

**規模の見積り**: 実測ではこの再構築は編集 tick の約23%（`../perf-baseline.md`
「RESP-3 完了時」）。issue の「編集中の体感の主因」という表現は測定に支持されない。
主因は第2段（HIGH-04 / HIGH-05）側。

ただし `Evaluator::register` は登録と同時に**そのノードのキャッシュ破棄と
dirty マークも行っている**（`crates/ravel-core/src/eval.rs:519-548`）。
再登録をやめるなら、この無効化だけを行う経路を用意しないと編集が反映されない。

## 目標アーキテクチャ

「ドキュメント変更」と「評価結果到着」を別チャネルに分け、
パネルの再構築はドキュメントが実際に変わった時だけに限定する。

```text
評価結果到着 ──▶ ViewerFrame グローバル      ──▶ Viewer
             └─▶ NodeEvalTimings グローバル ──▶ NodeEditor（負荷表示の再描画のみ）
             （ProjectState の notify は起こさない）

ドキュメント変更 ──▶ mirror_epoch を bump ──▶ ProjectState notify
                                          └─▶ 各パネル: mirror_epoch を比較
                                                ├─ 不変 → 何もしない
                                                └─ 変化 → モデル再構築 + notify
```

`mirror_epoch` は「ドキュメントをミラーするパネルが表示している内容の世代」を
表す単一のカウンタで、`document_changed` / `replace_document` /
`set_active_composition` で bump する（既存の `revision` はロード整合性判定用の
まま触らない）。

## 実装単位

| ID | 単位 | 対象 issue |
|---|---|---|
| RESP-1 | 評価結果到着をパネル notify から切り離す | CRIT-01 |
| RESP-2 | ドキュメント世代でパネル再構築をゲートする | HIGH-07 |
| RESP-3 | パラメータ編集で GPU パイプラインを再コンパイルしない | HIGH-06 |

### RESP-1 評価結果到着をパネル notify から切り離す

`on_eval_update` の `cx.notify()` を削除する。合わせて、その notify に
暗黙に依存していた唯一の描画を明示購読へ移す。

- `project_state.rs:960` の `cx.notify()` を削除し、
  「評価結果はグローバル経由で届く」ことを doc コメントで固定する
- NodeEditor に `cx.observe_global::<NodeEvalTimings>()` を追加し、
  ノード負荷表示だけを再描画する（モデル再構築は伴わない）
- `NodeEvalTimings` の更新は `on_eval_update` の先頭、
  世代が古くて drop される更新でも行われる現在の順序を保つ

**完了条件**

- 評価結果を1回 publish したとき Viewer だけが更新され、
  Timeline / Outliner / MediaBin / Properties のモデル再構築が起きない
  ヘッドレステスト
- `NodeEvalTimings` の更新が NodeEditor に届くことのテスト
- 再生中に上記4パネルの再構築が0回であることを手元で確認

### RESP-2 ドキュメント世代でパネル再構築をゲートする

- `ProjectState` に `mirror_epoch: u64` と `pub fn mirror_epoch(&self)` を追加し、
  `document_changed` と `replace_document` で bump する。
  `set_active_composition` は文書を変えないが表示対象を変えるため、
  ここでも bump する（パネルは `ActiveComposition` も別途購読しているが、
  ゲートを通過させる必要がある）。ロード適用で bump しない `revision` は
  そのまま残す
- Timeline / NodeEditor / Outliner / MediaBin / Properties の
  `ProjectState` observer に、直前に処理した epoch を保持させ
  （共有ヘルパー `panels::MirrorEpoch`）、不変ならモデル再構築と
  `cx.notify()` をスキップする。ゲートは observer のクロージャ側に置く:
  同じ sync 関数はコンポジション切替や選択変更の経路からも呼ばれ、
  そちらを epoch で止めてはいけない。
  epoch を進めない notify（保存完了によるタイトル更新など）で
  パネルが再構築されなくなる
- NodeEditor の `refresh_from_document`：保持後の選択集合が現在の
  `CanvasSelection` と等しいとき `set_selected_nodes` を呼ばない。
  第2波の observer 起動を止める

**完了条件**

- epoch 不変の notify でパネルが再構築されないことのテスト
- ドラッグ 100 move あたりのパネル再構築回数が「変化があった回数」に
  一致することの確認
- File ▸ Open / New / Undo / Redo / コンポジション切替の後に
  各パネルが正しく再構築されることの回帰テスト

### RESP-3 パラメータ編集で GPU パイプラインを再コンパイルしない

3つの独立した修正を1単位にまとめる（どれか1つだけでは効果が出ない）。

1. **再登録のスキップ**
   `NodeProcessor` に `fn rebuild_on_node_change(&self) -> bool { true }` を追加
   （既定 true = 安全側）。ノード状態をキャプチャしない5つの GPU プロセッサで
   false を返す。`Evaluator` に
   - `pub fn processor(&self, node: NodeId) -> Option<&Arc<dyn NodeProcessor>>`
   - `pub fn invalidate_node(&mut self, node: NodeId)`（`register` の
     キャッシュ破棄部分を切り出したもの。`register` はこれを呼ぶ形に整理）

   を追加し、`GpuEvalHooks::sync` の `Params` 分岐では、既に登録済みの
   プロセッサが `rebuild_on_node_change() == false` を返すとき
   `invalidate_node` だけを行う。既定 true なので、新しいノード種を
   追加しても分類漏れで壊れない。
2. **検証をキャッシュの後ろへ**
   `ShaderManager::compile_source` でソースハッシュを先に計算し、
   キャッシュヒット時は `validate_wgsl` を飛ばす。検証は決定的なので
   同一ソースの再検証は純粋な無駄。検証失敗時に `sources` を更新しない
   現在の挙動は維持する。
3. **パイプラインの共有キャッシュ**
   `(シェーダハッシュ, エントリポイント, レイアウト, ワークグループサイズ)`
   をキーに `Arc<ComputePipeline>` をキャッシュする。`ShaderManager` が
   キャッシュを持ち、`compile_source` + `ComputePipeline::new` を1つの
   API に統合する。同種 N ノードが1パイプラインを共有するので、
   構造編集時の再コンパイル回数がノード数ではなくノード種類数に比例する。
   呼び出し元は5プロセッサ（rasterize は compute 1本 + raster 1本）と
   `ravel-gpu/tests/compute_invert.rs`。

**完了条件**

- パイプライン作成回数のカウンタを `ShaderManager` に持たせ、
  キャッシュキーの4要素（ソースハッシュ / エントリポイント / レイアウト /
  ワークグループサイズ）が**すべて同一**なら2回目以降が0、
  どれか1つでも異なれば別パイプラインになることのテスト
- スライダードラッグ相当の `Params` 同期でプロセッサが再構築されない
  ことのテスト（`rebuild_on_node_change() == false` の経路）
- `Params` 同期後も編集値が反映されること（`invalidate_node` の回帰テスト）
- `../perf-baseline.md` シナリオ (b)「blur radius スクラブ」を再測定し、
  結果を追記する

## 検証

- `mise run check`
- GPU アダプタが必要なテストは実機で実行する
- 単位ごとに `ravel-review` を通してから PR を出す

## 関連

- `issues/README.md`（第1段の定義とこの後の段）
- `issues/medium/ui-rendering.md`（パネル側の1回あたりコスト）
- `../perf-baseline.md`, `eval-render-performance-plan.md`
