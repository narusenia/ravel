# [HIGH-01] 評価器が全エッジを走査する — 隣接インデックスが無い

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-core / 評価器・グラフ |
| 該当 | `crates/ravel-core/src/eval.rs:868`, `:726`, `crates/ravel-core/src/graph.rs:532-548` |

## 現状

`Graph` はエッジを `im::HashMap<EdgeId, Edge>` としてのみ保持し、隣接情報を持たない。

- `eval_node` は入力エッジを `graph.edges().filter(|e| e.target == node)` で集める
  → 訪問ノードごとに O(E) の全走査。1回の pull で O(N·E)
- `mark_dirty_at` は dirty ノードごとに `graph.outputs_of(current)` を呼ぶ
  → これも毎回全エッジ走査。ソース付近の1パラメータ編集で O(dirty·E)
- `inputs_of` / `outputs_of` は呼び出しごとに `Vec` を新規確保

## 影響

1,000ノード / 1,500エッジのプロジェクトで、実処理の前に毎フレーム約150万回のエッジ検査。
`im` マップの非連続イテレーションを伴うためキャッシュ効率も悪い。
プロジェクト規模に対して二次的に悪化する。

## 修正方針

グラフバージョンをキーにした隣接インデックス（target→入力エッジ、source→出力エッジ）を構築・キャッシュ。
グラフは immutable なのでバージョンごとに1回計算すれば足りる。
`Graph` 内部に持たせるか `Evaluator` 側に持たせるかは実装判断。
`eval_node` と `mark_dirty_at` の双方をインデックス経由に変える。

## 検証

- 大規模グラフ（1000ノード規模）の評価ベンチマーク
- パラメータ編集1回あたりのエッジ検査回数を計測

## 関連

- [HIGH-02](HIGH-02-graph-eq-no-ptr-eq-fastpath.md), [HIGH-03](HIGH-03-params-resolved-per-visit.md)
- [medium/core-evaluator.md](../medium/core-evaluator.md)
