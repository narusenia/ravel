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
