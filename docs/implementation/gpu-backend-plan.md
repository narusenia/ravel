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
| `wgpu::` の出現は **324 箇所 / 18 ファイル**、`ravel-gpu` と `ravel-nodes` の 2 クレートのみ | 他 6 クレートは Cargo.toml にも wgpu を持たない |
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

1 単位 1 PR。`GPUBK-1`〜`5` は**バックエンドを 1 つも足さずに完結**し、
それぞれ単独で価値がある。

| ID | 単位 | 対象 | 依存 |
|---|---|---|---|
| GPUBK-1 | バインディング記述の型をバックエンド非依存に | `gpu_util.rs` の 3 ヘルパ + `rasterize` の複製解消 | — |
| GPUBK-2 | 宣言的ディスパッチ API と再利用（`MED-GPU-01`） | 7 コンピュートノード | GPUBK-1 |
| GPUBK-3 | `TextureKey` の形式・用途を自前型に | `texture_pool.rs` | GPUBK-1 |
| GPUBK-4 | 生ハンドルの公開を停止 | `GpuContext` / `ComputePipeline` / `GpuTexture` | GPUBK-2, GPUBK-3 |
| GPUBK-5 | ラスタライズとレンダーパスの抽象 | `rasterize/mod.rs`, `raster.rs` | GPUBK-4 |
| GPUBK-6 | リードバックとアップロードの抽象（`HIGH-04` を含む） | `transfer.rs`, `frame.rs` | GPUBK-4 |
| GPUBK-7 | シェーダ変換経路（naga の各バックエンド出力） | `shader.rs` | GPUBK-4 |
| GPUBK-8 | interop 出口（OFX / HW デコード用） | `ravel-gpu` | GPUBK-4 |
| GPUBK-9 | デバイス共有の契約と GPUI フォーク方針 | `device.rs`, GPUI | GPUBK-4 |
| GPUBK-10 | Metal バックエンド | `backend/metal` | GPUBK-5〜7 |
| GPUBK-11 | D3D12 バックエンド | `backend/d3d12` | GPUBK-10 |
| GPUBK-12 | Vulkan バックエンド | `backend/vulkan` | GPUBK-10 |
| GPUBK-13 | 文書更新 | 要件・仕様・`docs/dev/` | GPUBK-10 |

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

### GPUBK-4 生ハンドルの公開を停止

- `GpuContext::device()` / `queue()` / `adapter()` を削るか
  `pub(crate)` に落とす。`adapter_info()` は自前の情報型で返す
- `ComputePipeline::raw()` / `bind_group_layout()` を非公開にする
- `GpuTexture::texture()` / `create_view()` を非公開にする
- `instance()` は**残す方向で検討する** — デバイス共有（`from_instance`）の
  対になっているため。ただし戻り値を interop 出口（GPUBK-8）に寄せる

**完了条件**

- `ravel-gpu` の公開 API シグネチャに `wgpu::` が現れない
  （`GPUBK-8` の interop 出口を除く）
- `ravel-nodes` の `wgpu::` 出現が 0 になる
- `ravel-nodes` の Cargo.toml から `wgpu` 依存が消える
- `ravel-core` / `ravel-ui` / `ravel-app` に変更が無い

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

### GPUBK-7 シェーダ変換経路

- `naga` のバックエンド feature（`msl-out` / `hlsl-out` / `spv-out`）を追加
- 11 個の WGSL を各出力へ変換し、生成物を検証する経路を作る
- ビルド時埋め込み（REQ-GPU-002）と実行時コンパイル（REQ-GPU-003、
  REQ-PLUGIN-002 の WGSL プラグイン）の両方が各バックエンドで通ること

**完了条件**

- 11 個すべてが MSL / HLSL / SPIR-V へ変換できるテスト
- 変換失敗が理由付きのエラーになる
- ユーザー WGSL の契約（REQ-GPU-003）が変わらない

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

### GPUBK-9 デバイス共有の契約と GPUI フォーク方針

- 抽象が「外からデバイスを受け取る」形を持つことを契約として固定する
- GPUI をハードフォークして同じバックエンドに載せる方針・範囲・
  上流追従のコストを文書化する（実装は別 PR 群）
- `GPUCOMP-11`（`VIEWER_MAX_DIM` の引き上げ / ゼロコピー表示）をここで判断する

**完了条件**

- デバイス共有が維持されていることのテスト（REQ-GPU-001 の受入条件）
- フォーク方針が `docs/specifications/architecture.md` に書かれている
- `VIEWER_MAX_DIM` の判断根拠が `perf-baseline.md` にある

### GPUBK-10〜12 各バックエンド

Metal → D3D12 → Vulkan の順。Metal を先にするのは開発機が macOS で、
OFX の GPU Suite が Metal を要求するため。

**完了条件（各バックエンド共通）**

- 全 GPU ノードのゴールデンテストが wgpu バックエンドと一致する
- `perf_baseline` が動作し、wgpu バックエンドとの比較が記録される
- バックエンドを切り替えても `.ravprj` の出力がビット等価
  （`render-export-plan.md` の決定性要件）

### GPUBK-13 文書更新

- `REQ-GPU-001` の受入条件（HW デコードのゼロコピー条件）を実測に合わせる
- `docs/specifications/architecture.md` の GPU 層
- `docs/dev/` のノード追加手順（バインディング宣言の書き方）
- `docs/agent-api-reference.md` の `ravel-gpu` 公開 API

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
