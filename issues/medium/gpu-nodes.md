# medium — ravel-gpu / ravel-nodes（GPU パイプライン・ノード処理）

---

## MED-GPU-04 | perf | CPU ラスタライズ経路がプリミティブごと・フレームごとに全画面カバレッジバッファを確保、しかもビューアがこれを使う

**該当**: `crates/ravel-nodes/src/rasterize/mod.rs:677`, `:686-694`
（呼び出し元: `crates/ravel-app/src/eval_hooks.rs:140` の `RasterizeProcessor::from_node`、
合成ノードは `lib.rs:112-114`）

`raster_paths` はプリミティブごとに fill 用 `vec![0u8; w*h]` と stroke 用の2枚目を確保し、
`blend_coverage` は形状の bbox に関係なくプリミティブごとに全画面を走査する
— O(プリミティブ数 × 解像度) の確保と走査。

`GpuEvalHooks::finalize` は、自身が `GpuContext` とプールを保持し GPU ラスタライザも存在するのに、
ビューアが表示する Geometry 出力すべてに対して CPU プロセッサ（`from_node` → `gpu: None`）を
構築する。結果、シェイプ / スキャッターノードをプレビューしながらのスクラブは
毎フレーム zeno の CPU ラスタライズを回す。
合成 `rasterize` ノードも CPU 経路に固定されている（`lib.rs` コメント: ゴールデンテストで固定）。

**修正方針**: `finalize` で GPU ラスタライザを使う（コンテキストとプールは既にスコープ内）。
CPU 経路では再利用可能なカバレッジバッファを1枚確保し、
`blend_coverage` をプリミティブの bbox に限定する。

---

## MED-GPU-05 | perf | `ensure_gpu` が同一 CPU フレームを消費 GPU ノードごとに再アップロードする

**該当**: `crates/ravel-nodes/src/gpu_util.rs:59-78`

N 個の GPU ノードに供給される CPU 常駐 `FrameBuffer`（デコード済みメディアフレームなど）は
フレームあたり N 回アップロードされる。
`ensure_gpu` は呼び出しごとにプールテクスチャを取得して `upload_texture` し、
アップロード済みテクスチャはディスパッチ直後に解放され、元バッファと紐付けられない。
4K RGBA32F では冗長アップロード1回あたり約 132MB の PCIe / ユニファイドメモリ帯域。

**修正方針**: CPU→GPU 変換結果を値側にキャッシュする
（最初の GPU 消費側で `GpuFrameBuffer` に変換して評価器キャッシュに格納）。
またはフレーム内でソースバッファの `Arc` ポインタでメモ化する。

---

## MED-GPU-06 | debt | リードバックのステージングプールが共有 `CacheBudget` の外にある

**該当**: `crates/ravel-gpu/src/staging.rs`（`IDLE_BUDGET_BYTES = 256 MiB`）、
`crates/ravel-gpu/src/device.rs`（`GpuContext` がプールを保持）

`GPUBK-6`（#282）が入れたステージングプールは、アイドル分の上限を**自前の
256 MiB 定数**で持つ。`CACHE-3` が立てた「メモリの権威を 1 つに」という原則
（`ravel_core::cache_budget::CacheBudget` が VRAM / RAM / Disk を一元管理する）
の外にある唯一の GPU 側プールで、**ユーザーが設定でメモリ上限を下げても
ここには効かない**。

そうなっている理由は妥当で、単に安易な移し替えができないというだけ:

- ステージングは `COPY_DST | MAP_READ` すなわちホスト可視メモリ。`Tier::Vram` に
  計上すると `TexturePool` が*デバイス*テクスチャを*ホスト*バッファのために
  evict する誤った取引になる
- `Tier::Ram` に載せるには `GpuContext` が `SharedCacheBudget` を保持する必要が
  あり、`GpuContext::from_handles`（アプリのデバイス共有経路）まで含む構築 API の
  変更になる

実害の大きさ: 定常的に保持されるのは解像度ごと 1 本なので、1080p 表示なら
約 32 MiB、4K なら約 127 MiB。上限に達するのは複数解像度が同時に動く場合だけ。
変更前は毎フレーム確保・解放していたので**総量は増えるが churn は消えている**。

**修正方針**: `GpuContext` に `SharedCacheBudget` を渡し、
`Tier::Ram` の headroom をアイドル許容量にする（`TexturePool::with_shared_budget`
が `Tier::Vram` に対してやっているのと同じ形）。ロック順は
プール → 予算を守る。`GPUBK-9`（デバイス共有の契約）が構築 API を触るので、
そこに相乗りするのが安い。

---

## MED-GPU-07 | debt | wgpu が 2 本入っていて、デバイス共有が構造的に成立しない

**該当**: `Cargo.toml:40,44`（`wgpu` / `naga` を `zed-industries/wgpu` の
git rev に固定）、`Cargo.lock`（`wgpu` 2 エントリ / `naga` 2 エントリ /
`wgpu-core` `wgpu-hal` 各 2 エントリ）

依存グラフが 2 系統に割れている:

```
ravel-gpu  -> wgpu 29.0.3  (git: zed-industries/wgpu@357a0c5) + naga 29.0.3
gpui_wgpu  -> wgpu 29.0.4  (crates.io)                        + naga 29.0.4
```

git ソースと registry ソースは Cargo にとって別クレートなので、**同じ
バージョンでも型が別**。`.agents/rules/rust.md` はこれを名指しで禁じている:

> Reuse the workspace-pinned `wgpu` revision. **Do not introduce a second
> incompatible wgpu version into application-facing GPU paths.**

帰結が 3 つ:

1. **`REQ-GPU-001` の受入条件「UI 描画とコンピュートがデバイスを共有する」が
   構造的に達成できない。** GPUI の `wgpu::Device` は 29.0.4 の型、Ravel の
   それは 29.0.3-git の型で、`GpuContext::from_handles` に渡せない。
   実際 `from_handles()` と `GpuContext::instance()` は**呼び出し元が 0**
   （`GPUBK-4` の調査で判明）。デバイス共有の API は用意されているが
   配線できる状態になっていない
2. **`GPUBK-9`（デバイス共有の契約と GPUI フォーク方針）の前提が無い。**
   契約を書く前に 1 本にする必要がある
3. wgpu + naga + wgpu-core + wgpu-hal をビルドが 2 組コンパイルする。
   CI キャッシュ（`#211` で触れた 10 GB 上限）とローカルの両方に効く

**そもそもこの git 固定から得ているものが無い。** upstream との差分は
1 ファイル 23 行:

```
gfx-rs/wgpu v29 ... zed-industries/wgpu@357a0c5
  ahead 2, behind 4, files 1
  wgpu-hal/src/gles/egl.rs (+23/-1)  "Add XCB display handle support to EGL backend"
```

Linux の GLES/EGL 限定のパッチで、Ravel は GLES を使わない
（`gpu-backend-plan.md` の「非対象」に OpenGL が名指しされている）。
しかも upstream v29 より 4 コミット遅れている。`gpui-ce` 側も
`wgpu = "29.0.3"` を crates.io から取っており、このフォークを見ていない。

**修正方針**: `Cargo.toml` の `wgpu` / `naga` を crates.io の 29.0.4 に戻す。
Ravel が要る naga の feature（`wgsl-in` / `msl-out` / `hlsl-out` / `spv-out`）は
crates.io の 29.0.4 に**すべて存在する**ことを確認済み（`spv-out` と
`wgsl-in` は index の `features2` 側）。

**検証**: `Cargo.lock` の `wgpu` / `naga` / `wgpu-core` / `wgpu-hal` が
各 1 エントリになること。`mise run check` が通ること。GPU ノードの
ゴールデンが一致すること（wgpu の実装が変わるので**必ず確認する**）。
Windows / Linux の CI が通ること（EGL パッチを捨てるので Linux は要注意 —
ただし Linux は現状 CI 対象外）。
一本化できたら **`from_handles` 経由のデバイス共有が実際に配線できるか**を
確認し、`GPUBK-9` へ引き渡す。

**注意**: 将来 wgpu にパッチが要ると判断した場合（例: `wgpu-hal` の
`metal::QueueShared::raw` が private で OFX に `id<MTLCommandQueue>` を
渡せない件 — `gpu-backend-plan.md` の `GPUBK-8` 節）、フォークに戻す前に
**gpui-ce 側も同じ rev に揃える**こと。片側だけ動かすとこの状態に戻る。
