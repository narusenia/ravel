# ゼロコピー Viewer 表示 実装計画（HIGH-09 の残り）

> **Status**: Planned — 2026-08-10

対象 issue: [HIGH-09](../../issues/high/HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md)
の残り（GPU→CPU→GPU の往復そのもの）。
関連要件: REQ-GPU-001（デバイス共有）、REQ-INFRA-009（GPU バックエンドの内製化）、
REQ-UI-004（スコープ付きビューア）。
前提計画: [`gpu-backend-plan.md`](gpu-backend-plan.md)（`GPUBK-9` の判断）、
[`gpu-compositing-plan.md`](gpu-compositing-plan.md)（`GPUCOMP-11`）、
[`color-management-plan.md`](color-management-plan.md)（`CM-7` が表示変換を GPU へ移した）。

## 問題

評価が GPU で終わったフレームが、画面に出るまでに **GPU → CPU → GPU** を通る。

```text
評価（GPU テクスチャ）
  → リードバック（GPU→CPU）
  → CPU 側で BGRA バイト列を包む
  → GPUI がテクスチャとしてアップロード（CPU→GPU）
  → 描画
```

**同じ絵が 1 フレームに 2 回バスを渡る。** GPU に置いたまま描ければ、
どちらも要らない。

### 何が既に片付いているか

この issue が挙げた症状のうち **往復以外はすべて解決済み**。

| 症状 | 状態 |
|---|---|
| UI スレッドでの f32 → BGRA 変換 | ✅ `GPUCOMP-9`（#284）が評価ワーカーへ移した（`HIGH-08` 解決） |
| 変換そのものの CPU コスト | ✅ `CM-7`（#367）が GPU へ移した。CPU の per-pixel 処理は経路から消えた |
| リードバック実装（ステージング再利用・二重コピー） | ✅ `GPUBK-6`（#282）が `GPUCOMP-8` を回収 |
| 解像度上限（`VIEWER_MAX_DIM`） | ✅ `VRES-1`（#300）が撤去し係数モデルへ |
| **GPU→CPU→GPU の往復** | ❌ **これがこの計画** |

### 残っているコスト

`CM-7` 後の 1920×1080、交互測定（`perf-baseline.md`）で、**GPU 常駐フレームが
画面に届くまでが 2.14 ms**。CPU の per-pixel 処理はこの経路から消えているので、
**残っているのはリードバック・再アップロード・包みだけ**。

> **内訳はまだ無い。** 既存のリードバックの数字（`GPUBK-6` 後の 1080p で
> 約 2.2〜2.4 ms）は **`CM-7` 前の測定**で、表示変換が GPU へ移って
> リードバック量が 1 画素 16 バイト → 4 バイトになった後の値ではない。
> **総和より大きい部品を内訳として引用しない** — 混ぜて割合を出すと
> 数字が嘘になる。**`ZC-1` が総和と内訳を同じ実行で測り直す**のはそのため。

**「往復を消して何 ms 得られるか」は `ZC-1` の出力であって、この計画書の
前提ではない。** 上限は 2.14 ms（60 fps 予算 16.7 ms の 12.8%）で、
そこから GPUI 側のアップロードと包みを引いた分が得。

## 決定事項

### 障害は Ravel 側ではなく GPUI 側

**`MED-GPU-07`（`Cargo.lock` に wgpu が 2 本）は 2026-08-05 に解決済み。**
`wgpu` / `naga` / `wgpu-core` / `wgpu-hal` はいずれも 1 エントリで、
`ravel-gpu` と `gpui_wgpu` が同じ wgpu を参照する。**Ravel は他人のデバイスを
受け取れる**（`interop::context_from_wgpu` / `interop::wgpu_instance`、
`GPUBK-9` が契約として固定し、`crates/ravel-gpu/tests/device_sharing.rs` が
機械的に確認している）。

> **`HIGH-09` の個票と `gpu-compositing-plan.md` は「前提として `MED-GPU-07`
> の解消が要る」と書いているが、それは古い。** この計画と同じ変更で直す。

穴は **GPUI 側に 2 つ**（`architecture.md` の「デバイス共有との関係」）:

1. **gpui はレンダラのデバイスを公開していない。** アプリ側に向いた口は
   `App::set_gpu_requirements` と `gpu_specs()` だけ。`gpui_wgpu::WgpuContext` は
   instance / adapter / device / queue を `pub` で持つが `gpui` から辿れない
2. **macOS の gpui は wgpu ではない。** `gpui_wgpu` を使うのは Linux /
   Windows（feature）/ web で、macOS は `gpui_macos` の Metal ネイティブ
   レンダラ。**macOS には「共有すべき同じ `wgpu::Device`」が存在しない**

### 開発機が macOS なので (B) を採る

`architecture.md` が挙げる 2 択:

- **(A) デバイス公開アクセサを足す。** Linux / Windows では成立し、macOS には
  効かない
- **(B) macOS のレンダラを wgpu へ寄せる、または Metal レベルで interop する**

**(A) だけでは開発機で体感が変わらない。** 測定も実機確認も macOS で行って
いるので、(A) を先に入れても「効いているかどうか分からないもの」が増えるだけ。

### (B) は「レンダラの置き換え」ではなく「Metal レベルの interop」から始める

`gpui_macos` の Metal レンダラを wgpu へ書き換えるのは、**この計画の範囲で
背負える大きさではない**（gpui-ce のレンダラ全体の書き直しであり、上流追従の
コストが恒久化する — `architecture.md` の「形の制約」に真っ向から反する）。

代わりに**同じ Metal デバイス上でテクスチャを渡す**。`wgpu` の Metal
バックエンドと `gpui_macos` は、どちらも `MTLDevice` の上に立っている。

**2026-08-05 に前提が 1 つ好転した。** `wgpu` 29.0.4 が
`fix(metal): Restore the Queue::as_raw method`（#9560 / #9789）を含み、
`id<MTLCommandQueue>` が取れるようになった。上流 CHANGELOG が
「v29 で *removed without good reason*」と書いているとおり、これは設計判断では
なく回帰だった。**`MED-GPU-07` の副産物として記録されているが、ゼロコピー
表示との関係は誰も書いていない** — キューが取れることは、
「Ravel が描いたテクスチャを GPUI が読む前に完了を待つ」同期を書くための
前提そのもの。

### 測ってから決める段を最初に置く

**この計画は着手可能だが、実装前に測る単位を先頭に置く。** 理由:

- **`CM-7` 後の内訳が無い。** 手元にあるリードバックの数字は `CM-7` 前の
  もので、リードバック量が 1 画素 16 バイト → 4 バイトになった後の値ではない
- 往復を消しても、GPUI 側のアトラス churn（`HIGH-09` が挙げたもう 1 つの症状）が
  残るなら得は小さい
- **フォークのパッチは上流へ返せる形に保つ**のが `architecture.md` の制約で、
  返せる形かどうかは実装の前に決める必要がある

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| ZC-1 | 往復の内訳を `CM-7` 後の姿で測り直す（**判断ゲート**） | — |
| ZC-2 | gpui-ce に Metal デバイス / キューの取得口を足す | ZC-1 |
| ZC-3 | Ravel の出力テクスチャを GPUI のカスタム要素で描く | ZC-2 |
| ZC-4 | 同期と寿命（フレーム跨ぎの取り違えを起こさない） | ZC-3 |
| ZC-5 | Linux / Windows の経路（(A) のデバイス公開アクセサ） | ZC-3 |
| ZC-6 | 文書更新と `HIGH-09` のクローズ | ZC-4, ZC-5 |

### ZC-1 往復の内訳を測り直す（判断ゲート）

**`CM-7` 後の 2.14 ms を分解する。** リードバック / CPU 側の包み / GPUI の
アップロード / アトラスの確保・破棄。

- 1920×1080 と 3840×2160 の両方
- `ViewerResolution` の `Full` / `Half` / `Quarter`
- **交互測定で比を出す**（このマシンは loadavg が 4 を下回らない）

**完了条件**

- 内訳が `perf-baseline.md` に測定条件（loadavg、往復回数）付きで載る
- **「往復を消して何 ms 得られるか」の見積もりが数字で出る**
- 得が 60 fps 予算の 5% を下回るなら、**この計画を凍結する判断を書く**
  （`GPUCOMP-10` が「非同期リードバックは着手しない」と結論した前例と同じ形）

### ZC-2 gpui-ce に Metal デバイス / キューの取得口を足す

フォーク（`narusenia/gpui-ce` の `gpui-ce-compat`）に、**macOS の Metal
レンダラが使っている `MTLDevice` と `MTLCommandQueue` を返すアクセサ**を足す。

- **上流へ PR できる汎用 API の形に保つ**（`architecture.md` の「形の制約」）。
  Ravel 固有の分岐をフォークに置かない
- `set_always_on_top` / `observe_window_minimized` と同じ扱い —
  **アプリ側では原理的に書けないもの**なので線を越えてよい
- **`.agents/rules/rust.md` は pinned git dependency の変更を着手前の確認事項に
  している。** rev を上げるのはこの単位

**完了条件**

- macOS で `MTLDevice` / `MTLCommandQueue` が取れる
- **その受け口が `ravel-gpu` 側に定義されている。** `interop::context_from_wgpu`
  は `wgpu` のデバイスを受け取る口で、**生の Metal ハンドルは受け取れない** —
  macOS の gpui は wgpu ではないので、渡すものが違う。**Metal 専用の取り込み
  経路を新しく定義する**か、`wgpu` の `Device::from_hal` 相当で GPUI の
  `MTLDevice` の上に wgpu デバイスを立てるかを、この単位で決めて書く。
  どちらにせよ `interop` の許可クレート（`gpu-device-sharing` lint）と
  facade 規約（`gpu-facade-wgpu`）に収まる形にすること
- **GPUI が作ったデバイスの上で `ravel-gpu` の抽象 API が最後まで動く**ことの
  テスト（`crates/ravel-gpu/tests/device_sharing.rs` と同じ形）
- **上流へ出せる形になっている**（Ravel 固有の名前・分岐が無い）
- `cargo tree -i gpui` と `cargo tree -i wgpu` がそれぞれ 1 本
  （`MED-GPU-07` の再発を防ぐ。`architecture.md` の「上流追従のコスト」）

### ZC-3 Ravel の出力テクスチャを GPUI のカスタム要素で描く

**完了条件**

- Viewer が GPU テクスチャから直接描かれ、**リードバックが 0 回**になる
  （`GPUCOMP-7` のリードバック計数を流用）
- 絵が従来と一致する（**`CM-7` が定めた ±1 コードの基準**を使う。
  GPU 経路同士なので、より厳しくできるなら厳しくしてよい）
- `ViewerResolution` と `quality` に影響されない

### ZC-4 同期と寿命

**ここが本当の難所。** 別々のタイムラインに乗った 2 つの利用者が同じ
テクスチャを触る。

- Ravel の評価ワーカーが書き終わる前に GPUI が読むと、**古い絵か壊れた絵**が出る
- GPUI が描き終わる前に Ravel がテクスチャをプールへ返すと、**次のフレームが
  上書きする**
- `TexturePool` の寿命管理（`PooledTexture` は Drop で戻らず手動返却）と
  噛み合わせる必要がある

**完了条件**

- フレームを跨いだ取り違えが起きないことのテスト（連番の絵を流して順序を検査）
- **テクスチャがプールへ返るのは GPUI が読み終わった後**であることのテスト
- デバイス喪失・ウィンドウ再作成で破綻しない

### ZC-5 Linux / Windows の経路

`gpui_wgpu` を使う経路では、**同じ `wgpu::Device` を共有できる**ので Metal の
interop は要らない。`architecture.md` の (A)。

**完了条件**

- Linux / Windows でもリードバックが 0 回
- **プラットフォームで分岐するのは 1 箇所**（デバイスの入手方法のみ）で、
  描画側は共通

### ZC-6 文書更新と `HIGH-09` のクローズ

- `HIGH-09` を `issues/closed/` へ（**個票が挙げた症状がすべて解決してから**）
- `gpu-compositing-plan.md` の `GPUCOMP-11`、`architecture.md` の
  「デバイス共有との関係」、`gpu-backend-plan.md` の非対象節
- `perf-baseline.md` に往復除去後の実測
- **`MED-GPU-07` を前提として引用している古い記述を全部落とす**
  （`HIGH-09` の個票、`gpu-compositing-plan.md`）

## 非対象

- **`gpui_macos` の Metal レンダラを wgpu へ書き換えること。** 上流追従の
  コストが恒久化する。同じ `MTLDevice` の上で interop する
- **アトラス churn の解消**（`HIGH-09` が挙げたもう 1 つの症状）。カスタム要素で描けば
  アトラスを経由しなくなる見込みだが、**それは結果であって目標ではない**。
  `ZC-1` の測定で切り分ける
- **非同期リードバック**（`GPUCOMP-10`）。`GPUBK-6` の測定で不要と判断済みで、
  往復そのものを消すこの計画とは排他
- **表示変換**（`CM-7` で GPU へ移った）と**カラーマネジメント**（`CM-9`）

## 検証

- GPU テストは**アダプタが必要**。`CM-7` と同じく、アダプタ無しでは skip される
  ことを前提に、**実機で通す手順を文書に明記する**
- **リードバック回数**が主要な観測量（`GPUCOMP-7` の計数）。時間ではなく回数を
  pin する — 時間は負荷で動くが回数は動かない
- 性能を主張するときは交互測定で比を出し、測定条件を併記する
