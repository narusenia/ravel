# [HIGH-07] マウス移動ごとに document_changed の全カスケードが走る

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-app / ProjectState, 各パネル |
| 該当 | `crates/ravel-app/src/project_state.rs:762-774`, `crates/ravel-app/src/widgets/scrub_input.rs:184`, `crates/ravel-app/src/panels/viewer.rs:582-634` |

> **解決済み**: PR #192（2026-07-28）。`ProjectState::mirror_epoch` で5パネルの再構築をゲートし、同一 `CanvasSelection` の再 publish も止めた。修正方針3（dirty フラグでフレーム1回に合流）は未実施 — 残りは `issues/medium/ui-rendering.md` MED-UI-06。
> 設計は `docs/implementation/done/ui-responsiveness-plan.md`。

## 現状

スクラブティック / キャンバスドラッグの各 move が `apply_document` → `document_changed` を呼ぶ。
`document_changed` は毎回

- コンパイル済みチェーンを破棄
- レイヤー選択・メディア選択のプルーン
- `audio::sync_from_document`
- 評価要求
- パネル observer 5つへ notify（[CRIT-01](../critical/CRIT-01-eval-update-notifies-whole-workspace.md) と同じファンアウト）

さらに NodeEditor の refresh はグラフ差分を検出し、**変化が無くても** `CanvasSelection`
グローバルを再 publish する（`node_editor.rs:723-729`）。これが第2波のグローバル observer を起動する
（Viewer の `selection_sub` (`viewer.rs:226`) が `document_has_node` 走査、Outliner notify）。

評価側は latest-wins で合流されており健全（`eval_service.rs:157-181`）。問題は UI 側。

## 影響

マウス移動1回あたり O(ワークスペース全体) の作業。スクラブ・ドラッグ操作の入力レイテンシに直結。

## 修正方針

1. 保持セットが現在のグローバルと等しいとき `set_selected_nodes` をスキップ
2. Outliner / MediaBin / Timeline は再構築前に ProjectState のリビジョンカウンタを比較
3. パネル同期をイベントごとインラインではなく dirty フラグでフレーム1回に合流

## 検証

- ドラッグ 100 move あたりの各パネル `render` 回数を計測
- リビジョン不変時にパネル再構築が起きないテスト

## 関連

- [CRIT-01](../critical/CRIT-01-eval-update-notifies-whole-workspace.md)
- [medium/ui-rendering.md](../medium/ui-rendering.md) — コンパイル済みチェーンの不要破棄など
