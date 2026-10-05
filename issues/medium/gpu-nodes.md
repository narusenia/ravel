# medium — ravel-gpu / ravel-nodes（GPU パイプライン・ノード処理）

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
