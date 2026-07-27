# GPU 常駐ジオメトリ実装計画（`GpuGeometry`）

> **Status**: Planned — 2026-07-27 — **Phase 0 の測定で中止しうる**

対象: ジオメトリ属性を GPU 常駐で受け渡し、CPU↔GPU 往復を境界だけに
限定する。関連要件: REQ-CORE-009、REQ-GPU-001、REQ-GPU-003、
REQ-CORE-005。

**順序**: `per-instance-modulation-plan.md` の後、`particle-plan.md` の前。
変調でフィールドと属性のインターフェースが固まってから GPU に写し、
最初の実利用者であるパーティクルに間に合わせる。

## 問題

### 手元の数字は当てにならない

`perf-baseline.md` のシナリオ (c)（`shape.rect → scatter.grid(25×20=500)`）:

| 内訳 | 実測 |
|---|---|
| ジオメトリ評価（**キャッシュ温**、CPU） | 0.007 ms |
| CPU ラスタライズ | 37.75 ms |

0.007 ms は魅力的に見えるが、**この計測はキャッシュヒットのオーバーヘッドを
測っている**。同ファイルが「evaluator は構築済み・キャッシュ温」と明記して
おり、実際のフィールド評価も属性書き込みも走っていない。しかも 500
インスタンスという小さい規模。

つまり「CPU ジオメトリ評価は速い」という結論は**この計測からは出せない**。
出せるのは「キャッシュが効く限り安い」まで。変調をアニメーションさせれば
毎フレーム未キャッシュ評価になり、そこは未測定。

38 ms の犯人だったラスタライズは GPU 化済み（2.6 ms、約 19×）。
**次のボトルネックがどこかは分かっていない。だから Phase 0 で測る。**

### それでも書く理由

**同じ罠を一度踏んでいる。** `done/eval-render-performance-plan.md` は
#65〜#69 の 5 PR をかけて、ノード単位の CPU↔GPU 往復を剥がす作業だった。
実測で 1 tick 14.6 ms のうち **8.8 ms（約 60%）が往復**。

ジオメトリはこれから同じ構造に入る。

```text
CPU: shape → scatter → field 評価 → 属性書き込み
                                      │  毎フレーム全属性をアップロード
                                      ▼
GPU: rasterize（instanced-quad）
```

500 インスタンスでは不可視。10 万インスタンスでは支配的になり、その頃には
CPU 経路が本番で動いていて剥がすのが高い。

**パーティクルが強制する。** GPU シミュレーションなら状態は GPU 常駐の
ポイントジオメトリになる。ジオメトリが CPU 専用型しか持たないと、
`particle.simulate` は毎フレーム読み戻すことになり、GPU を使う理由が消える。

### ただし勝手に作らない

`eval-render-performance-plan` の Phase 0 は「数字を取ってから作る」を掲げ、
実際に仮説を 1 つ潰している（`sync_processors` が重いという想定に対し
実測 0.57 ms）。本計画も同じ規律に従い、**Phase 0 の測定結果しだいで
中止する**。

## Phase 0: 測定（作る前に測る）

### 計測シナリオ

`crates/ravel-nodes/examples/perf_baseline.rs` を拡張する。

**全シナリオを未キャッシュで測る。** 既存のシナリオ (c) がキャッシュ温
だったために「ジオメトリは 0.007 ms」という誤読を生んだので、ここでは
パラメータをフレームごとに変えてキャッシュを外す（＝変調をアニメーション
させた実使用に相当）。参考としてキャッシュ温も併記する。

| # | 構成 | インスタンス数 |
|---|---|---|
| A | `shape.rect → scatter.grid` | 500 / 10k / 100k / 1M |
| B | A + `field.falloff → field.apply(scale)` | 同上 |
| C | B + `field.noise` と `field.attribute` の 3 段合成 | 同上 |
| D | C → `rasterize`（GPU）まで含む end-to-end | 同上 |

各シナリオで **CPU 評価時間・アップロード時間・GPU 時間**を分けて記録する。
シナリオ B / C は `per-instance-modulation-plan.md` の完了が前提
（未完了なら手組みのフィールドチェーンで代用する）。

### 判断基準

- **D の end-to-end が 10 万インスタンスで 16.6 ms（60fps 予算）を
  超える場合** → 本計画を実施する。超過分の内訳（CPU 評価 /
  アップロード / GPU）で単位の優先順を決める。
- **超えない場合** → 本計画を中止し、測定結果だけ `perf-baseline.md` に
  残す。パーティクル側（`particle-plan.md`）の GPU 判断は別途行う。
- **アップロードが CPU 評価より支配的な場合** → 全面 GPU 化ではなく
  「属性列のアップロードを差分化する」だけで足りる可能性がある。
  その場合は本計画を縮小して単位 1 のみ実施する。

**完了条件**: 上表の測定結果を `perf-baseline.md` に追記し、実施 / 縮小 /
中止のいずれかを本ファイルの Status に記録する。

## 目標構成

`GpuFrameBuffer` の設計をそのまま写す。

```rust
// 既存（画像）: crates/ravel-gpu/src/frame.rs
pub struct GpuFrameBuffer { ctx, inner: Arc<PooledHandle>, width, height }

// 新規（ジオメトリ）
pub struct GpuGeometry {
    ctx: GpuContext,
    /// 属性列ごとの storage buffer（ドメイン × 属性名）
    columns: HashMap<(Domain, AttrName), Arc<PooledBuffer>>,
    /// プリミティブ範囲とインスタンスソースは CPU 側メタデータのまま
    primitives: Arc<Vec<Primitive>>,
    instance_sources: Vec<Arc<Geometry>>,
    counts: DomainCounts,
}
```

受け渡しは `gpu_util` の既存パターンを踏襲する。

```rust
// 既存: crates/ravel-nodes/src/gpu_util.rs
pub enum GpuInput<'a> {
    Resident(&'a GpuFrameBuffer),   // 借用
    Uploaded(PooledTexture),        // CPU 入力を一時アップロード
}
```

`GpuGeometryInput` を同型で足し、CPU の `Geometry` が来たら
アップロードして包む。**下流ノードは常駐かどうかを知らなくてよい。**

### 読み戻し境界

`GpuFrameBuffer` と同じく、CPU 化するのは真の境界だけ。

| 境界 | 理由 |
|---|---|
| CPU 専用ノード | `attribute.transfer`（近傍探索）、`attribute.path_sample` |
| 属性スプレッドシート | 構造上 CPU で値を読む（`attribute-spreadsheet-plan.md`） |
| ゴールデンテスト | CPU 参照経路が検証のオラクル |
| エクスポート | 決定的出力 |

### CPU 経路は参照実装として残す

`rasterize` が既に GPU / CPU の二経路を持ち、ゴールデンテストは CPU 側で
回している（`procedural-geometry.md` の GPU 方針）。同じ形にする。
**CPU を削除しない。** 二重実装のコストは、決定性検証のオラクルを
持ち続ける対価として払う。

### 属性列だけを GPU に置く

`Primitive::Path { verts, closed }` の範囲情報と `instance_sources` の
入れ子構造は CPU 側メタデータのまま残す。GPU に置くのは**数値列だけ**。
トポロジまで GPU に持ち込むとデバッグ不能になるうえ、現状のラスタライザも
パス頂点を storage buffer にアップロードして範囲は uniform で渡している。

## 実装単位

Phase 0 の結果しだいで縮小・中止する。以下は「実施」判断が出た場合。

### 単位 1: `GpuGeometry` 型と転送

- `crates/ravel-gpu/` に `GpuGeometry` とバッファプール
  （既存 `TexturePool` と同型の `BufferPool`）。
- アップロード / 読み戻し（`AttributeArray` ↔ storage buffer）。
  型ごとのレイアウト規約（Vec2 → `vec2<f32>`、Color → `vec4<f32>`、
  Bool → `u32`、Str は **GPU 非対応**で CPU 残留）。
- `gpu_util` に `GpuGeometryInput`。

**完了条件**

- 全 `AttributeType`（Str を除く）のアップロード → 読み戻し往復で
  元の値と完全一致するテスト。
- `Str` 列を含むジオメトリが、Str 列だけ CPU 残留で他は常駐になるテスト。
- 常駐入力を借用したとき**アップロードが起きない**ことのテスト
  （`GpuFrameBuffer` の先例と同じ検証形）。
- バッファプールの再利用テスト。

### 単位 2: フィールドの WGSL 評価

REQ-GPU-003 の「フィールドの WGSL 評価」に相当。

- `Field` トレイトに WGSL ソース片を返す任意メソッドを足す。
  実装しないフィールドは CPU フォールバック。
- `field.noise` / `field.falloff` / `field.curve_remap` /
  `field.attribute` / `field.constant` / 二項合成の WGSL 版。
  ベクタ場（`vector-field-plan.md`）が入っている場合は戻り値が
  `vec2<f32>` になるだけで、1 フィールド = 1 WGSL 関数の構造は変わらない。
- `field.apply` の combine（`per-instance-modulation-plan.md` 単位 1 の
  `CombineMode` と成分マスク）を 1 カーネルにまとめる。

**完了条件**

- 各フィールドで CPU 版と GPU 版の出力が許容誤差内で一致するテスト。
  **これが二重実装を持つ意味なので、全フィールドで回す。**
- WGSL 未実装フィールドが CPU フォールバックする（結果が正しい）テスト。
- 合成チェーンが 1 パスにまとまることの検証（dispatch 回数）。

### 単位 3: 生成ノードの GPU 化

- `scatter.grid` / `scatter.circular` / `scatter.path_array` /
  `scatter.scatter` の GPU 生成。
- `geometry.transform`。
- `shape.*` は点数が少ない（数十〜数百）ので **CPU のまま**。

**完了条件**

- 各ノードで CPU / GPU の出力一致テスト。
- `scatter.scatter` の seed 決定性が GPU でも保たれるテスト
  （ハッシュ関数を CPU / WGSL で一致させる）。
- Phase 0 のシナリオ A〜D を再測定し、改善幅を `perf-baseline.md` に記録。

### 単位 4: 文書更新

- `docs/specifications/procedural-geometry.md`: GPU 方針節を実装に
  合わせて更新（「v1 は CPU SoA」の記述が現状と食い違うため）。
- `perf-baseline.md`: Phase 0 と単位 3 の測定を記録。

## 検証

- **CPU / GPU 一致テストが本計画の中心**。全フィールド・全生成ノードで回す。
- GPU アダプタが無い CI では GPU テストがスキップされる。
  一致テストがスキップされたままマージされないよう、
  **アダプタありの実機確認をマージ条件に含める**。
- 決定性: `scatter.scatter` のハッシュを CPU / WGSL で一致させるのは
  地味に難しい（浮動小数の丸め）。整数ハッシュに寄せて回避する。

## 非対象

- **トポロジの GPU 化**。`Primitive` と `instance_sources` は CPU 残留。
- **`Str` 属性の GPU 化**。仕様上「低頻度用途」と明記されている
  （`procedural-geometry.md` の制約節）。
- **`attribute.transfer` の GPU 化**。近傍探索は空間分割構造が要るので
  別スコープ。
- **タイポグラフィのシェーピング**。rustybuzz / swash は CPU。
- **パーティクルシミュレーションそのもの**。`particle-plan.md` の管轄。
  本計画はその土台を用意するだけ。
- **CPU 経路の削除**。参照実装として維持する。
