# GPU 常駐ジオメトリ実装計画（`GpuGeometry`）

> **Status**: In progress — 単位 1 実装済み（#638、2026-11-10）。Phase 0 実施済み（2026-07-29）: 判断は
> 「実施」**。ただし**支配的コストは計画の想定と違う**。測定結果と優先順の
> 変更は `perf-baseline.md`「ジオメトリ評価スケーリング baseline」と
> 下記 Phase 0 節に記録。

対象: ジオメトリ属性を GPU 常駐で受け渡し、CPU↔GPU 往復を境界だけに
限定する。関連要件: REQ-CORE-009、REQ-GPU-001、REQ-GPU-003、
REQ-CORE-005。

**順序**: `done/per-instance-modulation-plan.md` の後、`particle-plan.md` の前。
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
シナリオ B / C は `done/per-instance-modulation-plan.md` の完了が前提
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

### 測定結果と判断（2026-07-29）

数字は `perf-baseline.md`「ジオメトリ評価スケーリング baseline」。
判断は **実施**。10 万インスタンスの D が直列 18.24〜31.36 ms
（5 回実行、最速の実行でも min 16.82 ms で予算超過）。

**想定と違った点が 3 つある。**

1. **支配的なのは `raster_flatten`**（CPU でのインスタンス展開）で、
   10 万インスタンスの CPU 側 11.32 ms のうち **8.75 ms（77%）**。
   アップロードは 1.20 ms、CPU フィールド評価は 1.15 ms しかない。
2. **したがって「アップロードの差分化だけに縮小」は選べない。** 判断基準の
   3 番目の条件（アップロードが CPU 評価より支配的なら単位 1 のみ）は、
   両者が同程度で、かつ両方が展開コストに埋もれているため成立しない。
3. **CPU フィールド評価は 10 万では問題ではない**（C で 1.17 ms）。
   1M で 11.92 ms になるので、単位 2（WGSL フィールド）が効くのは
   50 万要素超、またはフィールド段数が増えたとき。

**優先順**: 単位 1 → **単位 5** → 単位 3 → 単位 2 → 単位 4。
単位 5 は今回の測定で追加した単位で、`flatten` の除去そのもの。
単位 1 は単位 5 の前提（常駐した属性列がないと展開先が無い）。

**パーティクルと 3D への影響**は `perf-baseline.md` の該当節に記録し、
それぞれ `particle-plan.md` / `3d-scene-plan.md` に反映した。要点:

- パーティクルの CPU ステップは 10 万点 0.2 ms 前後で予算の 1〜2%。
  **GPU sim（`particle-plan.md` 単位 6）の根拠はステップ高速化ではなく、
  状態を VRAM 常駐にして展開とアップロードを消すこと** = 本計画の単位 1 と 5。
- 3D の毎フレーム頂点アップロードは数十万頂点までなら成立する
  （6.5〜7.9 GB/s 実測）。**3D を本計画に依存させない判断は妥当**。

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
| 属性スプレッドシート | 構造上 CPU で値を読む（`done/attribute-spreadsheet-plan.md`） |
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

**実装メモ**

- **`BufferPool`**（`ravel-gpu/src/buffer_pool.rs`）。キーは
  `BufferKey { capacity, usage }`。`capacity` は **2 の冪のサイズクラス**
  （下限 256 B）で、バイト数そのままではない。散布の個数はパラメータで
  変わるので、完全一致キーだと編集のたびに再確保になる。代償は最大 2 倍の
  余白で、**論理長は列側（`GpuColumn::len`）が持つ**。アイドル台帳は
  `TexturePool` / staging と同じ `LruBudget`。VRAM tier へは
  `VramAllowance`（予算 + 共有プールのアイドル合計）経由で載り、
  `TexturePool` と `BufferPool` を同じ `VramAllowance` から作ると、両者の
  アイドルの合計が headroom に収まる（独立に読むと最大 2 倍保持する）。`TexturePool` との共通化は `LruBudget` までに留めた —
  `TexturePool` は pending-use 判定と bind group 退避を持つ別物で、
  共通の `Pool<K, T>` に畳むとその 2 つが型引数に漏れる。
- **長寿命の再利用**: 借りた `PooledBuffer` を保持し続ければ、それが
  そのまま「同一性による再利用」（静的メッシュの頂点バッファ等）。プールは
  保持者のいるバッファを回収しない。3D（`3d-scene-plan.md`）が静的メッシュに
  使う場合も同じ型。
- **レイアウト表**（`ravel-gpu/src/geometry.rs`、`column_stride` /
  `column_wgsl_type`）: `F32`→`f32`(4) / `Vec2`→`vec2<f32>`(8) /
  `Vec3`→`vec3<f32>`(**16**、WGSL の配列 stride。`w`=0 で書き、読み戻しで
  捨てる) / `Vec4`・`Color`→`vec4<f32>`(16、`Color` は `r g b a` 順) /
  `I32`→`i32`(4) / `Bool`→`u32`(4、読み戻しは非 0 を `true`) /
  `Str`→**GPU 非対応**。今ある `AttributeType` は 8 種で計画時と同じ。
- **CPU 残留は `Str` に加えて `Detail` ドメイン全体**。1 属性 1 値の
  ために 256 B のバッファを持つのは無駄で、シェーダは uniform で受ける。
  常駐するのは点・プリミティブ・インスタンスの数値列。
- **`GpuGeometry` の構成**: `columns: HashMap<(Domain, AttrName), GpuColumn>`
  と、常駐列を抜いた CPU `Geometry`（`host`）。計画の
  `primitives` / `instance_sources` / `counts` を個別フィールドにせず
  `host` に任せたのは、メッシュの index blob と画像ソースも同じく CPU 残留で、
  別フィールドにすると `Geometry` の分割ロジックを二重に持つため。
  `Domain` に `Hash` を足した（`ravel-core`、挙動変更なし）。
- **評価器での流れ**: `GpuFrameBuffer` と同じく `DataTypeId::GEOMETRY` を
  共有し、`is_gpu_resident() == true`、`byte_size` は VRAM（バッファ容量 +
  host）。**永続形式・DataType は増やしていない**。消費側は `GpuGeometry` を
  先に downcast し、無ければ `Geometry` へ落とす
  （`gpu_util::ensure_gpu_geometry` / `ensure_cpu_geometry`）。
  今 `Geometry` だけを downcast している既存ノードは、常駐値が流れ始めた時点で
  `ensure_cpu_geometry` を通す必要がある — **まだ何も `GpuGeometry` を
  生成しないので、現時点で挙動は変わらない**。
- **アップロード回数のカウンタ**: 既存の `GpuContext::transfer_stats()`
  （`uploads` / `readbacks`）に列ごとに 1 回ずつ計上する。借用で増えないことは
  `ravel-nodes` の `a_cpu_geometry_uploads_once_and_a_resident_one_is_only_borrowed`
  が固定している。
- **pending-use**: 池のバッファを dispatch バッチが使っている間は
  `acquire` が飛ばす（`GpuContext::mark_buffer_in_flight` が印、`flush` が
  消す）。上書き（`upload_buffer`）と書込み済みバッファの読み戻しは先に
  flush する。バインドする単位 2 / 5 は印を付けるだけでよい。
- **範囲外のまま残したもの**: 池のバッファを `dispatch_compute` /
  ラスタライザへバインドする経路と、アプリ側での `BufferPool` 生成
  （`TexturePool` と同じ `VramAllowance` から作る）は、最初の消費者である
  単位 2 / 5 が足す。
- ビッグエンディアン対象は `compile_error!` で拒否（テクスチャ転送と同じく
  ネイティブ順のキャストで GPU に渡すため）。

### 単位 2: フィールドの WGSL 評価

REQ-GPU-003 の「フィールドの WGSL 評価」に相当。

- `Field` トレイトに WGSL ソース片を返す任意メソッドを足す。
  実装しないフィールドは CPU フォールバック。
- `field.noise` / `field.falloff` / `field.curve_remap` /
  `field.attribute` / `field.constant` / 二項合成の WGSL 版。
  ベクタ場（`vector-field-plan.md`）が入っている場合は戻り値が
  `vec2<f32>` になるだけで、1 フィールド = 1 WGSL 関数の構造は変わらない。
- `field.apply` の combine（`done/per-instance-modulation-plan.md` 単位 1 の
  `CombineMode` と成分マスク）を 1 カーネルにまとめる。

**完了条件**

- 各フィールドで CPU 版と GPU 版の出力が許容誤差内で一致するテスト。
  **これが二重実装を持つ意味なので、全フィールドで回す。**
- WGSL 未実装フィールドが CPU フォールバックする（結果が正しい）テスト。
- 合成チェーンが 1 パスにまとまることの検証（dispatch 回数）。

**実装メモ（2026-11-10）**

- **WGSL の組み立て**（`ravel-core/src/geometry/field_wgsl.rs`、プレリュードは
  `field_prelude.wgsl`）。`Field::wgsl(&self, &mut FieldWgsl) -> Option<WgslFn>`、
  既定は `None`。**1 フィールド = 1 関数**
  `fn field_fn_N(p: vec2<f32>, i: u32) -> T`（位置 `p` と要素番号 `i`）。
  合成子は先にオペランドの `wgsl` を呼んでから自分の関数を出す。位置を引数に
  取る形にしたのは `field.gradient` / `field.curl_noise` が**オフセットした位置で
  ソースを再評価する**ため。木全体が 1 モジュール・1 ルート関数になるので、
  合成チェーンは**1 dispatch**（`field.apply` の combine / 成分マスク / group も
  同じカーネルに入る）。純粋な文字列と数値だけで GPU 型を知らないので、
  `ravel-core` に置いた（`ravel-gpu` が `ravel-core` に依存するため逆は不可）。
- **構造はソース、数値はデータ**。周波数・時計・カーブの制御点などは
  `FieldProgram::params()` の 32 ビット語として storage buffer で渡し、シェーダは
  `pf(k)` / `field_params[k]` で読む。生成ソースは木の形だけで決まり、
  パラメータを動かしてもパイプラインは増えない
  （`identical_fields_reuse_one_pipeline_while_their_parameters_change`）。
  `field.time` は時計を **CPU で 1 回読んで**値として渡すので、CPU と同じ値を使う。
- **フォールバック規則**: `wgsl` が `None` のフィールド、**および CPU が
  警告して既定値に落ちる構成すべて**（未知の属性、長さ違い、`Str` 列、列に無い成分、
  スカラ要求の所へベクタ、結合できないオペランド型）は `None` を返す。木のどこかが
  `None` なら木全体が CPU で評価される。CPU が基準で、エラーも警告も
  すでに正しく出すので、GPU 側で再実装しない。**部分木ごとのフォールバック
  （CPU で評価した列を GPU に渡す）はしていない**: `gradient` / `curl_noise` は
  オフセット位置が要るので部分木を列にできず、規則が 2 つになるため。
- **式フィールド（`field.expression`）は CPU のまま**。式言語（関数・属性参照・
  `f64` 評価）を WGSL へ移すのは小さくなく、2 つの評価器の保守が一生ついて回る。
  式はバッチあたり 1 回評価で済む場合（属性を読まない）が多く、そこは CPU でも安い。
  GPU に載せたくなったら式 IR からの生成として別単位で切る。
- **`field.attribute` の `normalize` も CPU**: 列全体の min/max
  という縮約が要り、要素ごとの関数にならない。
- **属性の結合上限 `MAX_FIELD_ATTRIBUTES = 3`**。属性 1 本が storage バインド 1 つで、
  compute ステージの保証は 8。`field.apply` のカーネル自身が 5
  （`P`・旧ターゲット・新ターゲット・group・パラメータ）使うので残りが 3。
  超える木は CPU。
- **`ravel-gpu` の足し物**: `BufferDispatch` / `BufferBinding` と
  `GpuContext::dispatch_buffers`（uniform が `@binding(0)`、バッファが
  `@binding(1..)`、`BindingKind::StorageBuffer` = 読み書き）。**記録時に束ねた
  プールバッファを `mark_buffer_in_flight` する**ので、呼び側は足場バッファを
  記録直後に `release` してよい。`GpuColumn::{upload, adopt, read}` と
  `GpuGeometry::{with_column, attribute_info}`。`column_wgsl_type` は
  `ravel-core` の `wgsl_element_type` に委譲し、レイアウト表と生成シェーダが
  食い違えないようにした。要素数は uniform で渡し `arrayLength` は使わない。
- **`field.apply` の経路**（`ravel-nodes/src/field/gpu.rs` の `GpuFieldBackend`）。
  常駐入力（`GpuGeometry`）は可能なら常にこの経路で、結果は `GpuGeometry`
  （ターゲット列以外を共有）、**読み戻しなし**。CPU の `Geometry` は
  `GPU_MIN_ELEMENTS = 1 << 16` 要素以上のときだけ GPU に送り、カーネルが読む列
  だけをアップロードし、ターゲット列だけを読み戻す。閾値は実測
  （Apple GPU、release、負荷のあるマシンで目安）: `noise(3 oct) * falloff + 定数`
  で CPU は 1 万 0.5 ms / 5 万 1.8 ms / 10 万 3.4 ms / 100 万 36.7 ms、GPU は
  CPU 入出力込みで 1.6 / 1.6 / 1.8 / 5.7 ms（常駐なら 100 万でも 1.7 ms）。
  単純な木ほど交差が遅いので、5 万の交差の少し上に置いた。
  **無いターゲットは GPU 経路が作る**（`create_if_missing` が既定で `true` なので
  初回変調が普通のケース）。型は `created_column_type`、中身は `created_column`
  と、CPU の作成規則を**共有**する（定数を写さない）。CPU 入力はホストで作って
  アップロード、常駐入力はホストで作る（導出色 = 点の `Cd` がパスの色に従う場合
  だけ、従う色列を読み戻す。定数の既定なら読み戻しなし）。
  GPU が受けない呼び出し（`Detail` ドメイン、型の合わないターゲット、作成して
  `amount == 0`、`create_if_missing` オフの無いターゲット、Bool でない group）は
  `Ok(None)` で CPU の `apply_field` が答える（エラーと警告は CPU が基準）。
  `amount == 0` は dispatch せず入力をそのまま返す（「変調なし」はビット一致）。常駐入力が CPU に
  落ちたときは読み戻して `Geometry` を返す（「読み戻し境界」）。
  `GpuApplyFieldProcessor` が `ApplyFieldProcessor`（CPU の基準実装、常駐入力は
  読み戻して処理）を包む。
- **配線**: `GpuEvalHooks` が `shared_pools_with_budget` で `TexturePool` と
  `BufferPool` を**1 つの `VramAllowance` から**作る（単位 1 のメモが
  「最初の消費者が足す」とした部分）。`processor_for_node_with_fields` /
  `register_all_processors_with_fields` が `GpuFieldBackend` を受け、
  従来の関数は `None`（CPU のみ）で委譲する。
- **パーティクルが呼ぶ入口**: `FieldProgram`。粒子ドメインの属性に対して
  `FieldProgram::compile(field, ctx, &lookup, len)` し、力カーネルのモジュールに
  `FieldProgram::wgsl(first_binding)` を入れ、`params()` と属性列を宣言された
  バインドに渡し、要素ごとに `FieldProgram::root_call("p", "i")` を呼ぶ。
  `field.apply` のカーネル（`apply_kernel`）はその最小の利用例。
- **NaN / 非有限入力**（レビューで発覚して修正）: `curve_eval` / `ramp_eval` は
  CPU が NaN で末尾の出力 / 色を返し、点 / 停止点が 1 つなら範囲外を引かない。
  WGSL はコンパイラが NaN を仮定しなくてよいので `x != x` ではなく
  `(bitcast<u32>(x) & 0x7fffffff) > 0x7f800000` で判定する（`is_nan_bits` /
  `is_finite_bits`、`unit_vector` の有限判定も同様）。
- **許容誤差と残る不一致**: 各フィールドの許容誤差は
  `tests/gpu_field_parity.rs` の表に明記（相対 1e-6〜5e-3。属性読み出しは 0 =
  ビット一致、`curl_noise` / `gradient` は有限差分が GPU の `sin` 等の誤差を
  `1/(2·step)` 倍するため 5e-3）。GPU の積和融合・`length`（Rust は `hypot`）・
  `atan2` は ULP 単位で違う。**NaN / ±inf の挙動は WGSL では規定されない**ので
  GPU と CPU の一致は有限値でのみ保証する（`atan2` のゼロベクトルは
  `atan2_ieee` で IEEE の符号付きゼロ規則に合わせた）。
- **検証の注意**: パリティテストはアダプタが無いと `skipping` で通る。
  アダプタ無しでも走るのは WGSL の naga 検証
  （`the_wgsl_of_every_field_validates_without_an_adapter`）だけ。

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

### 単位 5: `rasterize` が常駐ジオメトリを直接読む（CPU 展開の除去）

**Phase 0 の測定で追加した単位。実施順は単位 1 の直後**（単位 2 / 3 より先）。

現状の GPU ラスタライザは毎フレーム CPU で `flatten_geometry` を回し、
インスタンスとパスを `Vec<DrawItem>` + 頂点列に展開してから storage buffer を
作り直す。10 万インスタンスで 8.75 ms、100 万で 104 ms。**これが
ジオメトリ経路の最大コスト**で、要素数に線形。

- `GpuGeometry` の属性列を**そのまま**バインドし、インスタンスとパスの展開を
  頂点シェーダ / compute 側で行う。展開に必要な範囲情報
  （`Primitive::Path { verts, closed }`、`instance_sources` の入れ子）は
  「属性列だけを GPU に置く」方針どおり uniform / 小さな index buffer で渡す。
- 入れ子インスタンスは現状 CPU 側で深さ上限付きに再帰している。GPU 側では
  **1 段目までを直接展開し、2 段目以降は CPU 展開に落とす**形から始める
  （既存の深さガードの意味を保つ）。
- 点ドメイン（パーティクル）は展開が不要なので、まず点で入れて
  インスタンスを後にする分割が安い（点の `flatten` は 10 万で 2.29 ms、
  インスタンスは 8.75 ms）。

**`done/image-instancing-plan.md` が入ると本単位の前提が変わる。** instance source に
テクスチャという新種が混ざり、「属性列だけを GPU に送る / トポロジは CPU 残留」
では説明できなくなる。詳細は同計画書の「未解決の依存」を参照。

**完了条件**

- 既存の CPU / GPU 一致テスト（`rasterize` の
  `gpu_matches_cpu_for_paths_points_and_nested_instances` 相当）が
  常駐入力でも通る。
- 常駐入力で `raster_flatten` が呼ばれないことのテスト（span ではなく、
  展開を通らない経路を選んだことの検証）。
- 入れ子インスタンスの深さガードが GPU 経路でも効くテスト。
- Phase 0 のシナリオ D を再測定し、10 万インスタンスで 16.6 ms を
  下回ることを `perf-baseline.md` に記録。

### 単位 4: 文書更新

- `docs/specifications/procedural-geometry.md`: GPU 方針節を実装に
  合わせて更新（「v1 は CPU SoA」の記述が現状と食い違うため）。
- `perf-baseline.md`: Phase 0（記録済み）と単位 5 / 単位 3 の再測定を記録。

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
