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

## MED-GPU-11 | bug | `geometry.blast` でプリミティブを消すと、消した図形の頂点が点として描画される

**該当**: `geometry.blast`（`domain = primitive`）→ `rasterize`

プリミティブ削除は**点を残す**仕様（`procedural-geometry.md` のノード表）。
残った孤立点（どのプリミティブにも参照されない点）を `rasterize` が描くため、
消した rect の 4 隅に 1px 弱の点が見える。

**再現**: `shape.rect`（180x120）→ `attribute.set`（primitive、bool `kill` = true）→
`geometry.merge`（ellipse・star と合流）→ `geometry.blast`（primitive、group `kill`）→
`rasterize`。フェーズ F のショーケース生成器（`test/node-showcase-project` ブランチの
`crates/ravel-cli/examples/showcase_project.rs`、Geometry Ops コンプ）で目視確認
（2026-10-07）。

**実害**: 中程度。「消したはずのものが残って見える」形で出力に混ざる。

**修正方針**: 原因を先に切り分ける — (a) `rasterize` が孤立点を描くのが意図か
（点群の可視化として）、(b) primitive 削除で参照を失った点を落とすべきか。
(a) が意図なら `blast` の primitive 削除に「未参照の点も消す」オプションを足し、
既定をどちらにするか決める。
