# [HIGH-02] 編集ごとに全レイヤーネットワークを deep compare する（`Graph::eq` にポインタ比較が無い）

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-core / グラフ・コンポジション |
| 該当 | `crates/ravel-core/src/graph.rs:1170-1182`, `crates/ravel-core/src/composition/mod.rs:926-952`, `crates/ravel-core/src/eval.rs:565-697` |

> **解決済み**: `RESP3-2`（PR #395）。核だった 1 行
> （`old_layer.network != layer.network`）が `Graph::ptr_eq` で短絡するように
> なり、`Graph::eq` 自体も 3 段の短絡（マップ全体 → マップ → ノード `Arc`）を
> 持つ。`set_document` の祖先チェーン再構築はレイヤーごとの O(L) `get_layer`
> から親索引経由の O(1) ルックアップになった。

> **一部進展（2026-08-03 確認）**: `Graph::ptr_eq`（`graph.rs:704`）と
> comp 単位の短絡（`changed_network_paths` の
> `Arc::ptr_eq(comp, old_comp)`、`composition/mod.rs:1039`）は**入っている**。
> 残っているのは**変更のあった comp の中でのレイヤー単位 deep compare**
> （`composition/mod.rs:1046` の `old_layer.network != layer.network`）で、
> ここが `Graph::ptr_eq` を使っていない。本項目の核はこの 1 行なので未解決のまま。

## 現状

`Evaluator::set_document` は新ドキュメントを伴う `EvalRequest` ごとに走る。
コンポジション内の任意の編集で comp の `Arc` が差し替わり、その後
`changed_network_paths` が **そのコンポジションの全レイヤー**について
`old_layer.network != layer.network` を評価する。

`Graph::eq` はノードを `v.as_ref() == ov.as_ref()` で比較 — ポート・パラメータ
（キーフレームカーブ全体や `PathPoints` ベクタを含む）・再帰サブネットの完全な deep compare。
構造共有により無変更レイヤーの `Arc<Node>` はポインタ同一なのに、その事実を使っていない。

同じ `set_document` ブロックはレイヤーごとに O(L) の `get_layer` で祖先チェーンを再構築する
（編集あたり O(L²·depth)）。

## 影響

シェルのスライダー（不透明度・トランスフォーム）をスクラブすると変更ティックごとにこれが走る。
編集レイテンシが「編集した量」ではなく「コンポジション全体のノード・キーフレーム総量」に比例する。

## 修正方針

1. `Graph::eq` でノード `Arc` を `Arc::ptr_eq` で先に比較（マップ全体の `im::HashMap::ptr_eq` も検討）
2. `changed_network_paths` で、旧スナップショットとマップルートを共有するレイヤーはスキップ
3. 祖先チェーン再構築を O(L) ルックアップから親インデックス経由に変更

## 検証

- 多レイヤー・多キーフレームのコンポジションでシェルパラメータ編集のレイテンシを計測
- 無変更レイヤーで deep compare が発生しないことをカウンタで確認

## 関連

- [HIGH-01](HIGH-01-evaluator-no-adjacency-index.md), [HIGH-07](HIGH-07-document-changed-cascade-per-mouse-move.md)
