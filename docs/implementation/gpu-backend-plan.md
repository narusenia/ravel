# GPU バックエンド内製化 実装計画（REQ-INFRA-009）

> **Status**: Planned — 2026-08-03

対象要件: REQ-INFRA-009（GPU バックエンドと UI 依存の内製化）。
関連: REQ-GPU-001、REQ-GPU-002、REQ-GPU-003、REQ-PLUGIN-001、REQ-PLUGIN-002。
引き受ける issue: `issues/medium/gpu-nodes.md` の `MED-GPU-01`。

## 問題

**Must の要件が別の Must に阻まれている。**

REQ-PLUGIN-001（OpenFX 統合）は GPU Render Suite を要求し、その Suite は
OpenGL / CUDA / Metal を前提とする。一方 REQ-GPU-001 は wgpu 固定だった。
wgpu の内側のデバイスをこれらの API の語彙で外に出す道が無いので、
**OFX の GPU 経路は CPU バッファ往復に落ちる**。それは
`issues/closed/HIGH-05`（シェル合成の CPU per-pixel）で 0 にしたリードバックを、
OFX ノード 1 つごとに再導入することを意味する。

性能側の動機も同じ場所を指している。`MED-GPU-01` は「ノードディスパッチごとに
queue submit（ブラーは 2 回）、ディスパッチごとにユニフォームバッファと
バインドグループを新規作成。`GpuTask` バッチング trait は実装ゼロ」。

## 現状の実測（2026-08-03）

移行の難所は**クレート分離ではなく型の隠蔽**にある。

### 巻き込まれない範囲

| 事実 | 含意 |
|---|---|
| `wgpu::` の出現は **334 箇所 / 324 行 / 18 ファイル**、`ravel-gpu` と `ravel-nodes` の 2 クレートのみ（`find crates -name '*.rs' \| xargs grep -o 'wgpu::' \| wc -l`。テストと examples を含む） | 他 6 クレートは Cargo.toml にも wgpu を持たない |
| `ravel-core` は GPU 型を名前で知らない。`NodeData` が `is_gpu_resident()` を持つだけ（`types.rs:158`） | 評価器・キャッシュ・グラフ・Composition は無変更 |
| `ravel-app` は GPU フレームを不透明ハンドルとしてのみ触る（`eval_hooks.rs`。`wgpu::` は 0 箇所） | パネル層・Viewer 経路は無変更 |
| ノードプロセッサの GPU 依存は `processor_for_node` の引数 3 つ（`&GpuContext`, `&mut ShaderManager`, `&Arc<Mutex<TexturePool>>`）に集約 | 差し替え点が 1 箇所 |

**この列を壊す変更は、抽象の切り方が間違っている兆候として扱う。**

### 隠蔽が必要な範囲

`ravel-gpu` が façade になっていない。

```
GpuContext::device() / queue() / adapter() / instance()   → 生ハンドル（device.rs:126-150）
ComputePipeline::raw() / bind_group_layout()              → 生ハンドル（compute.rs:94-100）
GpuTexture::texture() / create_view()                     → 生ハンドル
TextureKey { format: wgpu::TextureFormat,
             usage:  wgpu::TextureUsages }                → プールのキーが wgpu の語彙
```

### 既に factor されている部分

| ファイル | 状態 |
|---|---|
| `ravel-gpu/src/compute.rs` | `ComputePipeline::new` / `dispatch`、`PipelineCache::get_or_create` があり、**パイプライン生成とキャッシュは抽象済み** |
| `ravel-nodes/src/gpu_util.rs` | バインドグループ**レイアウト**が 3 ヘルパに factor 済み（`input_texture_layout_entry` / `output_storage_layout_entry` / `uniform_layout_entry`）。戻り値の型だけが wgpu |

### 各ノードが手書きしている部分

7 つのコンピュートノード（`blur` / `color_correct` / `merge` / `transform` /
`comp/merge` / `comp/opacity` / `comp/transform`）で**形が同一**。

```rust
// blur.rs:87-121 の形。7 ノードすべて同じ
create_buffer_init(... BufferUsages::UNIFORM ...)   // ユニフォームを毎回作る ← MED-GPU-01
input.create_view(&TextureViewDescriptor::default())
output.create_view(&TextureViewDescriptor::default())
create_bind_group(&BindGroupDescriptor { entries: [...] })  // 毎回作る ← MED-GPU-01
create_command_encoder(...)                        // ノードごとに submit ← MED-GPU-01
```

**外れ値は 2 つだけ。**

- `rasterize/mod.rs`（45 箇所）— ストレージバッファで頂点データを渡す。
  しかも `gpu_util.rs` のレイアウトヘルパを**複製**している
  （`texture_layout_entry` / `storage_texture_layout_entry` が `:443` にある）
- `raster.rs`（23 箇所）— 唯一のレンダーパイプライン（`RenderPipeline` /
  `RenderPassDescriptor` / `VertexState`）

### シェーダ

WGSL は **11 ファイル**。`naga` は wgpu とは独立したクレートで、すでに
ワークスペースの直接依存（`features = ["wgsl-in"]`）。

## 決定事項

### 抽象を先に挟み、wgpu をその下の 1 実装にする

**バックエンドを差し替える前に抽象を入れる。** 抽象なしで Metal へ書き換え
始めると、その間ワークスペースがビルドできない。順序は必ず

1. 抽象を定義し、wgpu 実装をその下に置く（**動作は不変**）
2. 各バックエンドを抽象の下に足す

とする。1 の各単位が単独でマージでき、テストが通ることを完了条件にする。

### 抽象化それ自体が MED-GPU-01 の修正になる形にする

宣言的なディスパッチ API は「バインディングの内容が同じなら再利用する」ことを
自然に許すので、バインドグループとユニフォームバッファのキャッシュが入る。
**バックエンドを 1 つも足さない時点で性能が改善する**単位から始める。

これは順序の根拠として重要で、「脱却のためだけの投資」を先頭に置かない。

### GPUCOMP-8 / 10 は本計画の結論待ちに落とす

`gpu-compositing-plan.md` の残り単位のうち、

| 単位 | 扱い |
|---|---|
| GPUCOMP-9（f32→BGRA を評価ワーカーへ） | **先に入れる。** CPU 側の話でバックエンド非依存 |
| GPUCOMP-8（ステージング再利用・二重コピー除去・wait 範囲） | 本計画の `GPUBK-6`（リードバック抽象）に吸収。wgpu の `map_async` 前提で組んで捨てるのを避ける |
| GPUCOMP-10（非同期リードバック） | 同上。`GPUBK-6` 完了後に測り直して判断 |
| GPUCOMP-11（`VIEWER_MAX_DIM` / ゼロコピー表示） | 本計画の `GPUBK-9`（デバイス共有と GPUI）に統合。GPUCOMP-11 の本文が「デバイス間 interop を別計画に切る」と既に書いている |

### デバイス共有の経路を壊さない

現行の `GpuContext::from_instance(instance, ...)`（`device.rs:104-106`）は
「UI と compute で wgpu instance を共有する」契約。抽象化後も**外から
デバイスを受け取れる形**を保つ。これにより GPUI のハードフォークを
Ravel 側の作業と独立に進められる。

### OFX の出口を抽象に持たせる

抽象は「バックエンド固有ハンドルを取り出す」明示的な出口を持つ
（Metal テクスチャ、D3D12 リソース、デバイスポインタ）。これは façade の
原則の例外なので、**OFX ホストと HW デコード（REQ-GPU-001 の
VideoToolbox / NVDEC 条件）だけが使う**ことを型と doc で示す。

## 目標構成

```text
ravel-core        (GPU 型を知らない。NodeData::is_gpu_resident のみ)
     │
ravel-nodes       (宣言的バインディング + dispatch。バックエンド型を触らない)
     │
ravel-gpu         ← 抽象境界。公開 API にバックエンド型が出ない
     │            ＋ 明示的な interop 出口（OFX / HW デコード専用）
     ├── backend/wgpu     (移行の起点。当面の既定)
     ├── backend/metal    (macOS)
     ├── backend/d3d12    (Windows)
     └── backend/vulkan   (Linux)

WGSL (11 ファイル) ──naga──→ MSL / HLSL / SPIR-V
```

## 実装単位

1 単位 1 PR。`GPUBK-1`〜`9` は**バックエンドを 1 つも足さずに完結**し、
それぞれ単独で価値がある。

表は着手順に並べる。**`GPUBK-4`（生ハンドルの公開停止）は前提ではなく
仕上げ**で、`GPUBK-5`〜`8` の後に来る。当初は `GPUBK-5` 以降が `GPUBK-4` に
依存する形で書いていたが、これは向きが逆だった: `GPUBK-5` の対象
`rasterize/mod.rs` は `ravel-nodes` 側にあり、`GpuContext::device()` /
`queue()`、`ComputePipeline::bind_group_layout()`、`PooledTexture::create_view()`
を使っている。先に `GPUBK-4` でそれらを閉じると `GPUBK-5` が書けなくなる。
façade を閉じられるのは、外から生ハンドルを要る者が居なくなってからである。
`GPUBK-6`（`transfer.rs` / `frame.rs`）と `GPUBK-7`（`shader.rs`）は
`ravel-gpu` 内なので `GPUBK-4` を必要としない。

| ID | 単位 | 対象 | 依存 |
|---|---|---|---|
| GPUBK-1 | バインディング記述の型をバックエンド非依存に | `gpu_util.rs` の 3 ヘルパ + `rasterize` の複製解消 | — |
| GPUBK-2 | 宣言的ディスパッチ API と再利用（`MED-GPU-01`） | 7 コンピュートノード | GPUBK-1 |
| GPUBK-3 | `TextureKey` の形式・用途を自前型に | `texture_pool.rs` | GPUBK-1 |
| GPUBK-5 | ラスタライズとレンダーパスの抽象 | `rasterize/mod.rs`, `raster.rs` | GPUBK-1, GPUBK-2 |
| GPUBK-6 | リードバックとアップロードの抽象（`HIGH-04` を含む） | `transfer.rs`, `frame.rs` | GPUBK-3 |
| GPUBK-7 | シェーダ変換経路（naga の各バックエンド出力） | `shader.rs` | GPUBK-1 |
| GPUBK-8 | interop 出口（OFX / HW デコード用） | `ravel-gpu` | GPUBK-5, GPUBK-6, GPUBK-7 |
| GPUBK-4 | 生ハンドルの公開を停止 | `GpuContext` / `ComputePipeline` / `PooledTexture` / `GpuFrameBuffer` | GPUBK-5〜8 |
| GPUBK-9 | デバイス共有の契約と GPUI フォーク方針 | `device.rs`, GPUI | GPUBK-4, MED-GPU-07 |
| GPUBK-14 | **wgpu 直叩きの取り分を測る（❓判断ゲート）** | `perf_baseline` | GPUBK-4, MED-GPU-07 |
| GPUBK-10 | Metal バックエンド | `backend/metal` | GPUBK-14 の結果 |
| GPUBK-11 | D3D12 バックエンド | `backend/d3d12` | GPUBK-10 |
| GPUBK-12 | Vulkan バックエンド | `backend/vulkan` | GPUBK-10 |
| GPUBK-13 | 文書更新（`GPUBK-14` の判定に関わらず必要） | 要件・仕様・`docs/dev/` | GPUBK-14 |
| GPUBK-15 | ディスパッチを 1 コンピュートパスに畳む | `compute.rs`, `dispatch.rs` | GPUBK-14 |
| GPUBK-16 | ブロッキング読み戻しの 1 ms 切り上げを回収 | `transfer.rs` | GPUBK-14 |

### GPUBK-1 バインディング記述の型をバックエンド非依存に

`gpu_util.rs` の 3 ヘルパが `wgpu::BindGroupLayoutEntry` を返しているのを、
自前の記述型に変える。同時に `rasterize/mod.rs:443` の複製ヘルパを消して
共有ヘルパに寄せる。

- 記述は「バインディング番号・種別（入力テクスチャ / 出力ストレージテクスチャ /
  ユニフォーム / 読み取り専用ストレージバッファ）・可視ステージ」だけを持つ
- `ravel-gpu` 側で wgpu の記述子へ変換する。変換は 1 箇所

**完了条件**

- `ravel-nodes` から `BindGroupLayoutEntry` / `BindingType` / `ShaderStages` /
  `TextureSampleType` / `StorageTextureAccess` / `BufferBindingType` の
  出現が 0 になる
- `rasterize` が独自のレイアウトヘルパを持たない
- 全 GPU ノードのゴールデンテストが変更前と一致する

### GPUBK-2 宣言的ディスパッチ API と再利用（MED-GPU-01）

7 ノードの同型コードを 1 つの API に畳む。**この単位が本計画で最初に
ユーザーに効く。**

- 「入力テクスチャ N 枚・出力ストレージテクスチャ 1 枚・ユニフォーム構造体 1 つ」
  を宣言してディスパッチする API を `ravel-gpu` に置く
- ユニフォームバッファは内容ハッシュで再利用し、毎回の `create_buffer_init` を消す
- バインドグループは（パイプライン, テクスチャ, バッファ）の同一性で再利用する
- テクスチャビューは `GpuTexture` 側にキャッシュする
- **1 フレーム 1 コマンドエンコーダにまとめる**（`GpuTask` の doc コメントが
  約束していた挙動。`MED-GPU-01`）

**完了条件**

- 7 ノードから `create_bind_group` / `create_buffer_init` /
  `create_command_encoder` / `create_view` の直接呼び出しが消える
- 同一パラメータの連続評価でバインドグループとユニフォームバッファの
  新規作成回数が 0 になるテスト
- 1 フレームの submit 回数が「GPU ノード数」から 1 に減るテスト
- `perf_baseline` の 10 レイヤー再生形で `MED-GPU-01` 由来のコストが
  減っていることを `perf-baseline.md` に記録する
- 全 GPU ノードのゴールデンテストが一致する

### GPUBK-3 `TextureKey` の形式・用途を自前型に

`TextureKey { format: wgpu::TextureFormat, usage: wgpu::TextureUsages }` を
自前の enum / ビットフラグにする。プールの同一性判定が wgpu の語彙から離れる。

**完了条件**

- `TextureKey` に wgpu 型が現れない
- 既存のプール共有・LRU・予算会計の挙動が変わらない
  （`CacheBudget` のテストが通る）
- `usage` の完全一致キーで共有が制限される既知の問題
  （`issues/low/backlog.md` の `LOW-GPU-04`）を悪化させない

### GPUBK-5 ラスタライズとレンダーパスの抽象

外れ値 2 つを扱う。

- `rasterize/mod.rs` のストレージバッファ経由の頂点データ受け渡しを
  宣言的記述に載せる
- `raster.rs` のレンダーパイプラインを抽象化する（唯一の描画経路）

**完了条件**

- ラスタライズのゴールデンテストが一致する
- レンダーパス経路に wgpu 型が漏れない

### GPUBK-6 リードバックとアップロードの抽象（HIGH-04）

`gpu-compositing-plan.md` の `GPUCOMP-8` を吸収する。抽象を切りながら
実装を直すので、二重作業にならない。

- ステージングバッファをサイズ別にプールする（`transfer.rs:162` の
  毎回 `create_buffer` を消す）
- `frame.rs` の `cast_slice(&raw).to_vec()` による二重コピーを消す
- デバイス全体待ちをやめ、対象リソースだけを待つ
- 非同期完了の表現をバックエンド非依存の形にする（`map_async` を
  そのまま外に出さない。`GPUCOMP-10` の判断材料になる）

**完了条件**

- リードバック 1 回あたりのステージング確保が 0 になるテスト
- 二重コピーが消えたことを確認するテスト
- `perf-baseline.md` に測定を記録し、`GPUCOMP-10`（非同期リードバック）が
  必要かどうかの判断根拠を書く

### GPUBK-7 シェーダ変換経路 — 済み（#283）

- `naga` のバックエンド feature（`msl-out` / `hlsl-out` / `spv-out`）を追加
- 11 個の WGSL を各出力へ変換し、生成物を検証する経路を作る
- ビルド時埋め込み（REQ-GPU-002）と実行時コンパイル（REQ-GPU-003、
  REQ-PLUGIN-002 の WGSL プラグイン）の両方が各バックエンドで通ること

**完了条件**

- 11 個すべてが MSL / HLSL / SPIR-V へ変換できるテスト
- 変換失敗が理由付きのエラーになる
- ユーザー WGSL の契約（REQ-GPU-003）が変わらない

> **2026-08-05 の訂正**: 「11 個の WGSL」は**ファイル数**で、翻訳単位の数では
> なかった。`premultiplied.wgsl` は他の 4 本（`blur` / `comp_merge_adjustment` /
> `comp_transform` / `transform`）が呼ぶ関数だけを持つ**断片**で、単体では
> valid WGSL ではない。パイプラインが実際にコンパイルするのは合成後のソースで、
> **意味のある翻訳単位は 10**。テストは合成後を対象にし、合成の要否は
> ファイル自身のコメント行で判定する。11 個すべてが 3 ターゲットへ変換できる
> ことは満たしている（`premultiplied.wgsl` 単体も entry point 0 の
> モジュールとして通る）。
>
> 実装は `crates/ravel-gpu/src/translate.rs`（`ShaderTarget` / `TranslatedShader`
> / `translate_wgsl`）。**HLSL と SPIR-V の生成物は実コンパイラ（`dxc` /
> `spirv-val`）に通していない** — 開発機に無いため。MSL は 11 本すべて
> `xcrun metal` でコンパイル確認済み。CI で検証するなら外部ツールチェーンの
> 導入判断が要る。変換結果のキャッシュも未実装で、REQ-GPU-002 の受入条件
> 「コンパイル済みシェーダのディスクキャッシュ」は未達（消費者が
> `GPUBK-10` 以降のため早すぎる最適化と判断した）。
>
> `GPUBK-10`（Metal バックエンド）への申し送り: MSL のスロット割り当て規約
> （エントリポイントごと、buffer / texture / sampler 別カウンタ、宣言順、
> 末尾に sizes_buffer）が `translate.rs` 内に閉じている。バインドグループ構築側で
> **同じ規約**を使う必要がある。

### GPUBK-8 interop 出口

OFX（REQ-PLUGIN-001）と HW デコード（REQ-GPU-001）のためだけの出口。

- バックエンド固有ハンドルを取り出す API を、用途を型名と doc で限定して置く
- 「façade の例外である」ことを doc コメントに明記し、
  `scripts/lint-patterns.sh` で一般ノードからの利用を禁止する

**完了条件**

- Metal テクスチャ / D3D12 リソース / デバイスポインタが取り出せる
- 一般のノードプロセッサから到達できないことを lint で担保する
- OFX ホスト計画（未着手）が必要とする形を満たしていることを
  REQ-PLUGIN-001 の受入条件と突き合わせて記録する

> **2026-08-05 の実装メモ**: 実装は `crates/ravel-gpu/src/interop.rs`
> （`NativeApi` / `NativeHandle<'a>` / `NativeDevice<'a>` / `NativeTexture<'a>`、
> 取得は `native_api` / `native_device` / `native_texture`）。
> **クレートルートへ re-export しない**のが設計の要で、呼び出し側が必ず
> `ravel_gpu::interop` と綴るため grep で見える。これに乗せた lint が
> `scripts/lint-patterns.sh` の `gpu-interop-escape` で、許可クレートは
> `ravel-gpu` 自身・`ravel-media`（HW デコード）・将来の `ravel-ofx` のみ。
> `ravel-nodes` と `ravel-core` はもちろん `ravel-app` / `ravel-ui` も違反になる。
>
> ハンドルは**借用ポインタ**で、`'a` は取り出し元（`GpuContext` /
> `GpuFrameBuffer`）の借用。テクスチャはプール管理なので、フレームを生かして
> おくことは借用検査の形式ではなく実際の要件（最後のクローンが落ちると
> プールが同じテクスチャを別のフレームへ配る）。取得関数が `unsafe` なのは
> `wgpu::*::as_hal` が `unsafe` だからで、safety 契約（解放禁止・D3D12 では
> `AddRef` されていない・直接投入した作業はディスパッチバッチから見えない）は
> doc コメントに書いてある。
>
> **OFX GPU Render Suite が要求する形との突き合わせ（REQ-PLUGIN-001）**
>
> | 要求される形 | 状態 |
> |---|---|
> | `id<MTLDevice>` | 取れる（`Device::as_hal::<Metal>().raw_device()`）。macOS 実機で確認 |
> | `id<MTLTexture>`（画像） | 取れる（`Texture::as_hal::<Metal>().raw_handle()`）。macOS 実機で確認 |
> | `ID3D12Device*` / `ID3D12Resource*` | 実装済み。`--target x86_64-pc-windows-msvc` のクロスチェックで**コンパイルのみ**確認、実機未検証 |
> | `id<MTLCommandQueue>`（`kOfxImageEffectPropMetalCommandQueue`） | **取れる（2026-08-05 更新）。** 起票時は「取れない」と書いた — 当時固定していた wgpu フォークの `wgpu-hal` が `metal::QueueShared::raw` を非公開にしていたため。`MED-GPU-07` で crates.io の **29.0.4** へ移した結果、この版が含む `fix(metal): Restore the Queue::as_raw method`（#9789）で `wgpu_hal::metal::Queue::as_raw()` が公開に戻り、D3D12 / Vulkan と揃った。`interop` に取得口はまだ置いていない（消費者と一緒に OFX ホスト計画で足す）。**ホストが Ravel と同じキューに積めるので、当初申し送りにあった「別タイムラインの同期コスト」は前提から外れる** |
> | CUDA stream / device pointer | **満たせない。** CUDA バックエンドが存在せず、本計画の非対象でもある（`GPUBK-10`〜`12` は Metal / D3D12 / Vulkan）。CUDA 経路しか持たない Windows のプラグインは CPU 経路に落ちる |
> | Vulkan の `VkImage` | 未実装。`VkImage` は非ディスパッチャブルな `u64` でポインタではなく、`NativeHandle` の形に入らない。`GPUBK-12` で別の型を足す |
>
> REQ-PLUGIN-001 の残りの受入条件（スキャン / ロード、Image Effect、
> パラメータ表示、プロセス分離、`kOfxStatErrUnsupported`）は GPU 抽象と
> 独立で、本単位は影響しない。「GPU Render が動作する」は macOS/Metal の
> 前提（device + texture）が揃った状態で、キューの扱いだけがホスト計画に残る。
>
> **REQ-GPU-001 との関係**: 出せるのは**エクスポート方向だけ**。HW デコードの
> ゼロコピー受け取り（VideoToolbox / NVDEC の出力を `GpuFrameBuffer` として
> 取り込む）は import 方向で、`create_texture_from_hal` 相当の別の口が要る。
> 消費者（`ravel-media` の HW デコード経路）が無い段階で形を決めると腐るので
> 本単位では置かない。よって REQ-GPU-001 の受入条件「macOS で VideoToolbox
> 経由の HW デコード出力を GPU メモリ上でゼロコピーで受け取れる」は**未達**で、
> 前進したのは「デコーダを構成する相手の device が名指しできる」ところまで。

### GPUBK-4 生ハンドルの公開を停止

`GPUBK-5`〜`8` が外部の呼び出し元を抽象へ移し終えた後に、façade を閉じる。

- `GpuContext::device()` / `queue()` / `adapter()` を削るか
  `pub(crate)` に落とす。`adapter_info()` は自前の情報型で返す
  （`adapter()` は呼び出し元が既に 0）
- `ComputePipeline::raw()` / `bind_group_layout()` を非公開にする
  （`raw()` は呼び出し元が既に 0）
- `PooledTexture::create_view()` と `PooledTexture.texture`、
  `GpuFrameBuffer::texture()` を非公開にする
  （`GpuFrameBuffer::texture()` は外部の呼び出し元が既に 0）
- `instance()` は**残す方向で検討する** — デバイス共有（`from_instance`）の
  対になっているため。ただし戻り値を interop 出口（GPUBK-8）に寄せる

**完了条件**

- `ravel-gpu` の公開 API シグネチャに `wgpu::` が現れない
  （`GPUBK-8` の interop 出口を除く）
- `ravel-nodes` の `wgpu::` 出現が 0 になる
- `ravel-nodes` の Cargo.toml から `wgpu` 依存が消える
- `ravel-core` / `ravel-ui` / `ravel-app` に変更が無い

`ravel-nodes` の `wgpu` 依存を消すには `examples/perf_baseline.rs` の
生ハンドル利用も畳む必要がある（`state_readback_ms` がバッファを直に作って
リードバック時間を測っている）。`GPUBK-6` がステージングの抽象を持たせる
時点で、この計測をその API 経由に書き換えるのが素直。

> **2026-08-05 の実装メモ**: 完了条件 4 つを満たした。閉じた口は
> `GpuContext::device()` / `queue()` / `instance()`（`pub(crate)`）、
> `adapter()`（削除）、`ComputePipeline::raw()`（削除）と
> `bind_group_layout()` / `dispatch()`、`PooledTexture.texture` と
> `create_view()`（後者は呼び出し元 0 なので削除）、
> `GpuFrameBuffer::texture()`。計画書が挙げていなかった 2 つも同じ理由で
> 閉じた — `CompiledShader.module`（`pub` フィールド）と
> `ComputePipeline::dispatch()`（複数行シグネチャで当初の grep から漏れていた）。
>
> **`adapter_info()` の自前型**は `AdapterInfo { name, vendor, device,
> device_type: DeviceType, driver, driver_info, backend: GpuBackend }`
> （`device.rs`）。`GpuBackend` は「今動いているバックエンド」で
> `Vulkan` / `Metal` / `Dx12` / `Gl` / `BrowserWebGpu` / `Noop` を取り、
> `interop::NativeApi`（「interop できる API」= Metal / D3D12 のみ）とは別物。
> `native_api` は前者から後者を導く。PCI バス ID とサブグループサイズは
> 消費者が無いので写していない。
>
> **`instance()` と `from_handles()` は `interop` へ移した**
> （`interop::wgpu_instance` / `interop::context_from_wgpu`）。どちらも
> 呼び出し元が 0 だったので移設に伴う変更は無い。計画書の「戻り値を interop
> 出口に寄せる」の素直な解釈で、デバイス共有は定義上「外からバックエンド固有の
> オブジェクトを受け取る」ことなので趣旨も一致する。**結果として
> `gpu-interop-escape` lint の対象**になり、GPUI のデバイスを共有する
> `ravel-app` は今のままだと違反する。許可クレートを広げるのか契約を別の形に
> するのかは `GPUBK-9` の判断で、それを lint に出したまま残すのが狙い。
>
> **`transfer.rs` は `&PooledTexture` を取る**形にし、`key` 引数を落とした
> （`upload_texture(ctx, &pooled, data)` /
> `read_texture(ctx, &pooled)` / `read_texture_shared` /
> `begin_read_texture`）。全呼び出し元が例外なくリースの `key` を渡していたので
> 引数は冗長で、リースから読めば**コピーのレイアウトと確保のレイアウトが
> 食い違えない**。
>
> **`compute_invert.rs` は公開 API だけで書き直した**（`ComputePipeline::new`
> + `GpuContext::dispatch_compute`）。**抽象に穴は見つからなかった** —
> エンコーダもバインドグループも submit も要らず、リードバックがフラッシュ点に
> なる。統合テストはクレート外の消費者なので、この形が「抽象が十分である」
> ことの常設の証明になる。
>
> **戻り防止の lint** は `scripts/lint-patterns.sh` の `gpu-facade-wgpu`。
> `crates/ravel-gpu/src` の公開シグネチャ・公開フィールド・公開定数に
> `wgpu` が現れたら落ちる（`interop.rs` は除外）。シグネチャは折り返すので
> 複数行検索にし、`{` で区切って本体へはみ出さないようにしている。
>
> **`perf_baseline.rs` の `state_readback_ms` は書き直さず削除した。**
> あれは**バッファ**の読み戻し（`copy_buffer_to_buffer` → マップ範囲を
> 64 B おきに走査、ステージングは計測区間外で 1 回確保）で、
> `ravel-gpu` の転送抽象は**テクスチャしか扱わない**。テクスチャ読み戻しに
> 置き換えると測る量が変わる（タイル解除を伴う `copy_texture_to_buffer`、
> 256 B 行パディング、疎な走査ではなくフルコピー — 16 MB では memcpy だけで
> 記録値 1.9〜2.1 ms に匹敵する）ので、`perf-baseline.md` の記録と比較
> できなくなる。数値を作り替えるより測れないことを記録する方を選んだ
> （`perf-baseline.md`「GPU 常駐状態の読み戻し」節に注記）。**再計測には
> バッファ読み戻しの抽象が要る** — 消費者は `GPU-1`（`GpuGeometry`）なので、
> 形はそこで決める。

### GPUBK-9 デバイス共有の契約と GPUI フォーク方針

- 抽象が「外からデバイスを受け取る」形を持つことを契約として固定する
- GPUI をハードフォークして同じバックエンドに載せる方針・範囲・
  上流追従のコストを文書化する（実装は別 PR 群）
- `GPUCOMP-11`（`VIEWER_MAX_DIM` の引き上げ / ゼロコピー表示）をここで判断する
  → 判断は済み、実装は `zero-copy-viewer-plan.md` が引き受けた。
  **macOS は完了**（#382 / #384 / #386）、Linux / Windows は `ZC-7` / `ZC-8`
  （#391）で完了

> **`GPUBK-4` が残した棘を先に解く。** デバイス共有の入口
> （`interop::context_from_wgpu` / `interop::wgpu_instance`）は `interop`
> にあるので、`ravel-app` がそれを呼ぶと `gpu-interop-escape` lint に
> 引っかかる。**許可クレートを広げる**のか、**契約を interop を経由しない
> 別の形にする**のかがこの単位の判断で、意図的に lint に出したまま
> 残してある（黙って許可すると façade の穴がもう一つ増える）。
>
> **`MED-GPU-07` を先に解消すること。** `Cargo.lock` に wgpu が 2 本
> （`ravel-gpu` → git の 29.0.3、`gpui_wgpu` → crates.io の 29.0.4）入って
> いて型が別なので、**そもそも GPUI の device を受け取れない**。
> 契約を書く前に 1 本にする。

**完了条件**

- デバイス共有が維持されていることのテスト（REQ-GPU-001 の受入条件）
- フォーク方針が `docs/specifications/architecture.md` に書かれている
- 当時の `VIEWER_MAX_DIM` の判断根拠が `perf-baseline.md` にある（定数は
  `VRES-1` が撤去し、係数ごとの実測は同じ文書の `VRES-5` の節にある）

### GPUBK-14 wgpu 直叩きの取り分を測る（❓判断ゲート）

`REQ-INFRA-009` は目的を 2 つ挙げ、どちらも第一級としている。OFX 対応と
**性能**。前者は `GPUBK-8` で裏付いた（interop 出口が無いと OFX の GPU 経路が
成立しないことは実装して確かめた）。**後者はまだ一度も測っていない。**

`GPUBK-1`〜`8` で得た改善は、記録されている限り**すべて wgpu の内側**で
取れている（`perf-baseline.md`）:

| 単位 | 効果 | 出どころ |
|---|---|---|
| `GPUBK-2` | submit 29 → 0.48 / 評価、`evaluate` −16%、blur 2.2× | ディスパッチの組み方（自分たちのコード） |
| `GPUBK-6` | readback 1080p −59〜61%、4K −72〜77% | ステージングプール・wait 範囲・二重コピー除去 |
| `GPUCOMP-10` | 測って**不要**と判断 | — |

**「wgpu の抽象を通すコスト」を測った数字が 1 つも無い。** wgpu 由来として
唯一特定されているのは `GPUCOMP-10` の分析にある「`wgpu-hal` Metal の
フェンス待ちが 1 ms 刻みに切り上がる」分で、**約 1 ms**。同じ分析が
「非同期化を持ち出さずスピンでも拾える性質のもの」と書いている。

Metal / D3D12 / Vulkan を 3 本書いて**永続的に保守する**対価が、この規模の
取り分に見合うのかは自明でない。この計画は `GPUCOMP-10` を測定で殺し、
`GPUCOMP-11` / `PATH-0a` を判断ゲートに置いてきた。**同じ規律を
`GPUBK-10` の手前にも置く。**

- 代表的なディスパッチ列（`perf_baseline` の 10 レイヤー再生形）について、
  wgpu 経由と**プラットフォーム API 直叩きの薄いプローブ**で同じ処理を回し、
  差を測る。バックエンドを完成させる必要は無い — 測りたいのは
  「wgpu の記述・検証・バリア挿入が 1 フレームあたり何 ms 積んでいるか」
- 測るのは macOS / Metal のみ（開発機であり、`GPUBK-10` の対象）
- **`MED-GPU-07`（wgpu 二重化）を先に解消してから測る。** 2 本入ったままだと
  どちらの wgpu を測っているか曖昧になる

**この計測が判定できるのは `GPUBK-10`（Metal）だけ。** wgpu が積むコストは
バックエンドごとに違う（D3D12 は記述子ヒープとリソース状態遷移、Vulkan は
明示バリアとディスクリプタセットで、wgpu の抽象が吸収している量が Metal と
同じ保証は無い）。**Metal の結果を D3D12 / Vulkan へそのまま横流ししない。**

- `GPUBK-11` / `GPUBK-12` は、`GPUBK-10` が**実際に着地したときの実測改善**を
  根拠に、各プラットフォームで同じ判断をやり直す。Metal で取り分が出たことは
  他の 2 つの必要条件でも十分条件でもない
- 逆に Metal で取り分が出なかった場合は、**3 つとも見送りに寄せてよい**。
  wgpu が最も薄いはずのバックエンドで差が出ないなら、より厚い抽象を挟んでいる
  D3D12 / Vulkan で差が出る可能性は残るが、その賭けに 2 本の永続保守を
  先払いする理由が無い（必要になったときに測り直せる）

**完了条件**

- 差が `perf-baseline.md` に日付付きで記録される（過去の記録は書き換えない）
- その数字に基づいて **`GPUBK-10` を**実施 / 縮小 / 見送りのいずれかに判定し、
  根拠を本節に追記する。`GPUBK-11` / `GPUBK-12` の判定は `GPUBK-10` の
  着地後に持ち越すことを明記する
- 見送る場合、`REQ-INFRA-009` の性能目的を取り下げる（OFX 対応と
  デバイス共有のためだけの計画に縮小する）ことを要件側にも反映する

**判定に関わらず残るもの**: `GPUBK-9`（デバイス共有の契約）は
`REQ-GPU-001` の受入条件なので性能とは独立に要る。**`GPUBK-13`（文書更新）も
無条件**で、見送った場合はその判断自体を要件と `architecture.md` に
書き戻す作業になる（`GPUBK-10` に依存しない）。

### 判定（2026-08-06）: `GPUBK-10` は **見送り**

実測は `perf-baseline.md` の「wgpu 抽象の取り分（`GPUBK-14` 判断ゲート）」節。
Apple M5 / macOS 26.3 / release / 512×512。容疑の 3 項目それぞれの結果:

| 項目 | wgpu の取り分 | バックエンドを書かないと取れないか |
|---|---|---|
| フェンス待ちの粒度 | 約 1.32 ms / ブロッキング待ち 1 回 | **取れる。** `wgpu-hal` の `thread::sleep(1ms)` が原因で、上流修正でも有界スピンでも回収できる。しかも Metal の `waitUntilCompleted` はスピンと同速（202 vs 212 µs）なので、コアを焼く判断を伴わない |
| ディスパッチ 1 件の encode | 158 µs / 評価（30.26 パス） | **ほぼ取れる。** 5.06 µs/パスの傾きは「ディスパッチごとに `begin_compute_pass`」という**我々のコードの構造**由来で、同じ wgpu で 1 パスに畳むと 0.09 µs/パス。バックエンド固有分は **8.5 µs / 評価**（`wgpu 単一パス` − `Metal エンコーダ/ディスパッチ`。158 = 149.5 + 8.5 で閉じる基準。native 側を `Metal 単一エンコーダ` に取ると約 20 µs だが、差は Metal 自身のエンコーダ刻みの費用） |
| 自動バリア挿入 | **測定誤差内（0）** | 空振り。Metal 側でエンコーダ境界を刻んだ場合と 1 エンコーダに畳んだ場合が全チェーン長で誤差内 |

**バックエンドを書いて初めて取れるのは 8.5 µs / 評価**（基準を
`Metal エンコーダ/ディスパッチ` に固定した値。`Metal 単一エンコーダ` 基準なら
約 20 µs で、差は Metal 自身のエンコーダ刻みの費用 — `perf-baseline.md` の
当該節に基準の取り方を書いた）。**どちらでも 60 fps 予算 16.7 ms の 0.1% 未満**で、
Metal / D3D12 / Vulkan の 3 本を**永続的に保守する**対価としては
成立しない。`GPUCOMP-10` を測定で殺したのと同じ規律をここにも適用する。

**この判定は性能の目的についてのみ。** `REQ-INFRA-009` のもう一方の目的
（OFX 対応）は `GPUBK-8` で裏付いており、`GPUBK-9`（デバイス共有の契約）も
`REQ-GPU-001` の受入条件として独立に残る。実務上この計画は
**OFX 対応とデバイス共有のための計画として進める**。

**要件側は「取り下げ」ではなく「格下げ」にした。** 上の完了条件は
「見送る場合、`REQ-INFRA-009` の性能目的を**取り下げる**」と書いていたが、
実際に反映したのは**第一級 → 副次的・要再測定**への格下げである
（`REQ-INFRA-009` は `Revised (v2)`）。理由は上表が **Metal のみの実測**で、
D3D12 / Vulkan を測っていないから。取り下げてしまうと、後で D3D12 か Vulkan で
取り分が出たときに要件を上げ直す手間が要るし、「測っていない」ことと
「成立しない」ことの区別が要件から消える。**格下げなら、性能を根拠に
バックエンドへ着手することは禁じたまま、未測定であることを要件に残せる。**
再び目的として立てるには各プラットフォームでの実測を先に出す
（Metal の結果を横流ししないという本節の規律と同じ）。

**`GPUBK-11` / `GPUBK-12` の判定は持ち越す。** 上表は macOS / Metal のみの
実測で、D3D12（記述子ヒープとリソース状態遷移）/ Vulkan（明示バリアと
ディスクリプタセット）で wgpu が吸収している量が同じ保証は無い。
**Metal で取り分が出なかったことを他の 2 つへ横流ししない。** 計画が書いている
とおり「3 つとも見送りに寄せてよい」が、それは *賭けに先払いしない* という
判断であって、D3D12 / Vulkan を測ったことにはならない。必要になった時点で
同じプローブを各プラットフォームで書いて測り直す。

**この判定から出た、バックエンドと無関係な作業 2 件**を `GPUBK-15` /
`GPUBK-16` として起票した（本単位では実装しない）。詳細は下の各節。

#### 完了条件からの逸脱（明記）

完了条件は「wgpu 経由と**プラットフォーム API 直叩きの薄いプローブ**で
**同じ処理**を回す」と書いているが、**実際に測ったのは狭いプローブで、
10 レイヤー合成チェーンの忠実な再現ではない**。

- 測ったのは 512×512 Rgba32Float のテクスチャ 2 枚を往復する自明なカーネル
  （読んで定数倍して書く）を、**実測した 30.26 パス / 評価**に合わせた長さで
  並べたもの。実際の `network → transform → opacity → merge` ではなく、
  WGSL → MSL の移植もしていない
- **したがって得られたのは原価項目ごとの差であり、フレーム全体の差は
  その積み上げによる推定**（約 1.48 ms / 評価、うち約 1.46 ms は wgpu 内で
  回収可能）。両経路でフレーム全体を回して得た差ではない
- 測っていないものの一覧は `perf-baseline.md` の同節末尾
  「このプローブで言えないこと」にある

**この逸脱が判定を変えるか**: 変えないと判断した。判定を支配しているのは
「バリアの取り分が 0」「encode の取り分の 95% が wgpu 内で回収可能」
「フェンス待ちは wgpu-hal の実装粒度」の 3 点で、いずれも
**チェーンの中身ではなく構造から出る**性質のもの。忠実な再現をすれば
カーネルの実行時間は伸びるが、それは両経路に等しく乗る。

### GPUBK-10〜12 各バックエンド

**`GPUBK-14` の判定が「実施」の場合にのみ着手する。**

Metal → D3D12 → Vulkan の順。Metal を先にするのは開発機が macOS で、
OFX の GPU Suite が Metal を要求するため。

> **抽象の形についての申し送り（`GPUBK-8` の実測から）**
>
> **バックエンドごとに「何が取れるか」が違うので、全バックエンドの
> 最小公倍数で統一型を決めると必ずどこかで破れる。** `GPUBK-8` の
> `NativeHandle`（`NonNull<c_void>`）は、置いた時点で既に 2 箇所で破れている:
>
> - **Metal のコマンドキュー**（`MED-GPU-07` で解消済み、教訓としては有効）。
>   当時固定していた wgpu フォークは `metal::QueueShared::raw` を非公開に
>   していたのに **D3D12 と Vulkan には `Queue::as_raw()` があった**。
>   同じ「キュー」がバックエンドによって取れたり取れなかったりしたわけで、
>   しかもそれは**上流の一時的な取りこぼし**（29.0.4 の #9789 で復帰）だった。
>   バックエンド差には「設計上の差」と「上流の穴」の 2 種類があり、
>   前者だけを型に写して後者は上流へ送る
> - **Vulkan の `VkImage`**。非ディスパッチャブルハンドルは仕様上 `u64` で
>   ポインタではない（32 bit 環境ではポインタサイズですらない）。
>   一方 `VkDevice` / `VkQueue` はディスパッチャブルでポインタなので入る。
>   **device は入るが image が入らない**という非対称になる
>
> `GPUBK-7` の `ShaderTarget` / `TranslatedShader` のように、**差を型で
> 表に出す**ほうが持つ。「どのバックエンドでも同じ形で取れる」を前提にした
> API は、次のバックエンドで例外を足すことになる。

**完了条件（各バックエンド共通）**

- 全 GPU ノードのゴールデンテストが wgpu バックエンドと一致する
- `perf_baseline` が動作し、wgpu バックエンドとの比較が記録される
- バックエンドを切り替えても `.ravprj` の出力がビット等価
  （`done/render-export-plan.md` の決定性要件）

### GPUBK-13 文書更新

- `REQ-GPU-001` の受入条件（HW デコードのゼロコピー条件）を実測に合わせる
- `docs/specifications/architecture.md` の GPU 層
- `docs/dev/` のノード追加手順（バインディング宣言の書き方）
- `docs/agent-api-reference.md` の `ravel-gpu` 公開 API

### GPUBK-15 ディスパッチを 1 コンピュートパスに畳む

`GPUBK-14` の測定から出た、**バックエンドを書かずに取れる**作業。
`ComputePipeline::dispatch`（`compute.rs`）が `begin_compute_pass` を
**ディスパッチごとに**呼んでいる。同じ wgpu で 1 パスに畳むと
パスあたりの傾きが **5.06 → 0.09 µs**（56 分の 1）になる。

- 再生形（10 レイヤー、30.26 パス / 評価）で **149.5 µs / 評価**
- **バリアの意味は変わらない** — wgpu はパス内のディスパッチ間でも依存を追う

**フレーム予算に対する位置**: 1080p / 10 レイヤーのフレームは約 15.8 ms
（`perf-baseline.md` の「ビューア経路の表示解像度」節）なので約 1%。
単体では小さいが、**`Full` が 60 fps に入るかは残余 0.9 ms で決まる**ため
`GPUBK-16` と合わせると効く位置にいる。

**完了条件**

- 記録パス数（`DispatchSnapshot.dispatches`）が畳み込み後に減ることのテスト
- **既存のゴールデンテストが無改変で通ること**（バリアの意味が変わっていない
  ことの機械的な確認。ここが本単位の主なリスク）
- `perf-baseline.md` に前後比較を日付付きで追記

### GPUBK-16 ブロッキング読み戻しの 1 ms 切り上げを回収

`wgpu-hal` Metal の `Device::wait` が「status を見る → 未完了なら
`thread::sleep(1ms)`」のループなので、**ブロッキング待ちが 1 ms 刻みに
切り上がる**（`GPUBK-14` の実測で wgpu 1523 µs 対 Metal 202 µs）。

- `wait_timeout` を使った**有界スピン → ブロック**、または上流修正
- **アプリは公開フレームごとに 1 回払っている**（`eval_hooks.rs` の `finalize`）
- Metal の `waitUntilCompleted` はスピンと同速（202 対 212 µs）なので、
  **「コアを焼くか 1 ms 待つか」のトレードオフではない**

**フレーム予算に対する位置**: 切り上げの損は 1 回あたり最大約 1 ms、
期待値で約 0.5 ms。1080p / 10 レイヤーの 15.8 ms に対し **3〜6%**。
`VRES` で `Half` を既定にするとフレームは約 5.7 ms へ縮むので、
**同じ絶対値が 7〜17% に上がる** — つまり `VRES` の後にやる方が取り分が大きい。

**完了条件**

- 有界スピンの上限を超えたときにブロックへ落ちることのテスト
- **CPU を焼き続けないことの確認**（スピンの上限が効いている）
- `perf-baseline.md` に前後比較を日付付きで追記。**`VRES-1` 後に測る**
- 上流修正を選ぶ場合は `gpui-ce-ravel` ではなく `wgpu` 側なので、
  **固定 git 依存の変更に当たるか**を確認する（`.agents/rules/rust.md`）

## 検証

- `GPUBK-1`〜`8` はヘッドレス。GPU アダプタを要する
- **ゴールデンテストが本計画の背骨。** 各単位で全 GPU ノードの出力が
  変更前と一致することを確認する。抽象化は挙動を変えない作業なので、
  一致しない場合は必ず退行
- **各単位でワークスペースのビルドとテストが通る**（REQ-INFRA-009 の受入条件）
- `GPUBK-10`〜`12` は該当プラットフォームの実機

## 非対象

- **OFX ホストの実装**（REQ-PLUGIN-001）。本計画は出口を用意するところまで
- **OpenGL / CUDA バックエンド**。OFX が要求するのは相互運用であり、
  Ravel 自身の描画経路としては採らない
- **GPUI のフォーク実装**。`GPUBK-9` で方針を決め、実装は別計画
- **ゼロコピー表示の実装**。`GPUBK-9` で判断し、必要なら別計画
- `GPUCOMP-10`（非同期リードバック）。`GPUBK-6` の測定後に判断
