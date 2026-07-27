# [HIGH-03] キャッシュヒット時でもパラメータを全再解決・再確保する（PathPoints を毎フレーム clone）

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-core / 評価器 |
| 該当 | `crates/ravel-core/src/eval.rs:1024-1027`（本体 `:1200-1246`） |

## 現状

`resolve_params` はキャッシュ有効判定の**前**に呼ばれる（新しい `NodeOutput` バインディング検出のため）。
しかし毎回 `ResolvedParams` 全体を再構築する。

- `Vec` 確保 + パラメータごとに `p.key.clone()`（String）— `:1244`
- `String` パラメータの clone — `:1209`
- `ParameterValue::PathPoints(points) => ResolvedValue::PathPoints(points.clone())` — `:1242`
  → 手描きパス（数百 `PathPoint`）を、そのノードを pull するたび、毎フレーム丸ごとコピー

その後キャッシュから返される場合でも、この確保は無駄になる。

## 影響

再生中、到達可能グラフのパラメータ総数に比例した定常的なアロケーションチャーン。
ペンツールで描いたパスを持つプロジェクトほど悪化する。

## 修正方針

1. キャッシュ有効判定に必要なのは `Channel*` / `NodeOutput` ソースのみ。定数は解決不要
   → 鮮度チェック用の部分解決と、実処理時の完全な `ResolvedParams` 構築を分離（遅延化）
2. `ParameterValue` / `ResolvedValue` の `PathPoints` を `Arc` 化し、解決をポインタ clone にする

## 検証

- パス付きノードを含むグラフでフレームあたりのアロケーション量を計測
- キャッシュヒット経路で `PathPoints` の clone が起きないテスト

## 関連

- [HIGH-01](HIGH-01-evaluator-no-adjacency-index.md), [medium/core-evaluator.md](../medium/core-evaluator.md)
