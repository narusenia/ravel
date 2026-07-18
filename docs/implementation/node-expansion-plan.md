# Node expansion plan — scalar math, geometry transform/merge, frame port

- **ステータス**: 実施中（2026-07-18 設計合意済み）
- **関連**: REQ-LAYER-002（In / Out インターフェース）、
  `docs/implementation/layer-network-model-plan.md`（完了済み前提基盤）

## 問題

param InputPort 化（layer-network 完結、#83〜#86）でパラメータをポート
駆動できるようになったが、駆動源を加工する手段がない。具体的には:

- `net.in` の `t` は秒単位のため、係数を掛けられずパラメータ駆動で
  実用的な動きが作れない（`t × 係数` ができない）。
- フレーム番号ベースの駆動（`f`）が取得できない。
- Geometry を生成後に移動・回転・拡縮する汎用ノードがなく、shape の
  center パラメータ頼みになっている。複数 Geometry の結合もできない。

## 対象アーキテクチャ

既存アーキテクチャの変更はなし。全ノードは現行 `NodeProcessor` シグネチャ
（構築時キャプチャなし、`ResolvedParams` 毎フレーム解決）で実装し、
`processor_for_node`（`crates/ravel-nodes/src/lib.rs`）と
`register_builtins`（`crates/ravel-core/src/registry/builtin.rs`）に登録する。

唯一の永続化影響は `net.in` への組み込みポート `f` の追加:

- **契約**: レイヤールートの In は `base_geometry` / `t` / `f` を常備出力
  する。サブネット内 In のポートはサブネットのピン境界なので対象外。
- **migration**: ロード時に `Document::normalize_net_in_ports()` が
  `f` を持たない In へ**末尾** append（エッジは `OutputPortIndex` 参照
  のため既存配線不変）。冪等。
- **衝突**: `f` という名前の legacy カスタムポート（同名パラメータを持つ）
  は migration がスキップし、評価器・Properties パネルとも
  カスタムパラメータ意味論を維持する（custom 優先）。

## 実装単位（1 単位 = 1 PR）

1. **`net.in` frame ポート `f`**（`feat/net-in-frame-port`）
   - 定数 + `NetInProcessor` 分岐 + layer-templates 4 種 + ロード時
     normalization + docs。
   - 完了条件: 新規レイヤーの In に `f` があり frame 番号を出力する。
     旧アーカイブがロードで `f` を獲得し、既存エッジ・legacy `f`
     カスタムポートが保全される（テストあり）。
2. **`math.scalar` + `math.remap`**
   - 単一 type_key + `op`（String enum）。2項:
     add / subtract / multiply / divide / min / max / mod / pow、
     1項: abs / negate / floor / ceil / round / sqrt / sin / cos
     （b 無視、ラジアン、0 除算→0、mod は `rem_euclid`）。
     `a` / `b` は Float パラメータ（InputPort 露出で駆動、固定入力
     ポートなし）。`math.remap` は線形 fit
     （in_min / in_max → out_min / out_max + `clamp` bool）。
   - 完了条件: `t` / `f` を露出 param port 経由で演算し shape パラメータ
     を駆動できる。ゼロ除算・退化 in 範囲で NaN を出さない（テストあり）。
3. **`geometry.transform`**
   - translate_x/y、rotation（度）、scale_x/y、pivot_x/y +
     use_centroid（既定 on）。適用順は pivot 基準 scale→rotate→translate
     固定。P へ CoW 適用、instance domain は P 変換 + ROT/SCALE 合成。
   - 完了条件: point / instance 両 domain の変換がテストで検証される。
4. **`geometry.merge`**
   - 固定 2 入力 A/B。属性は和集合 + 型付きゼロ埋め、primitive は
     頂点オフセット付け直しで連結。
   - 完了条件: 属性不一致・primitive 連結・空入力がテストで検証される。

各 PR は `mise run check` 通過 + ravel-review PASS をゲートとする。

## スコープ外（次バッチ以降）

- vector 系 generator / math、color 系ノード（型・ポート慣習の設計を
  先に行う）。
- カーブ版 remap（カーブ編集 UI とセットで設計。`field.curve_remap`
  との基盤共有を検討）。
- 可変長入力ポート基盤（merge の 3+ 入力等。graph モデル + エディタ UI +
  永続化を跨ぐため別計画）。
- time 系ノード（time remap / ループ等の具体需要が出るまで見送り）。
