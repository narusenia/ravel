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
| ZC-5 | Linux の経路（(A) のデバイス公開アクセサ） | ZC-3 |
| ZC-6 | 文書更新と `HIGH-09` のクローズ | ZC-4, ZC-5 |
| ZC-7 | Windows の経路（`ZC-5` から分離。**実機確認は手動**） | ZC-5 |
| ZC-8 | 起動時に GPUI のデバイスを採用する（`ZC-5` が露呈させた欠落） | ZC-5 |

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

### ZC-5 Linux の経路

`gpui_wgpu` を使う経路では、**同じ `wgpu::Device` を共有できる**ので Metal の
interop は要らない。`architecture.md` の (A)。

> **この単位は当初「Linux / Windows」だった。Windows は `ZC-7` へ切り出した。**
> 理由は下の `ZC-7` 節に書く。ここは Linux / FreeBSD だけを見る。

**完了条件**

- Linux でもリードバックが 0 回
- **プラットフォームで分岐するのは「テクスチャをどう名指すか」だけ**で、
  何をいつ描くか・いつ諦めるかは共通

> **2 つ目は当初「デバイスの入手方法のみ」と書いていた。実装が示した形に
> 合わせて改めた。** macOS は不透明ポインタ＋完了コールバック、wgpu 経路は
> `Arc<wgpu::Texture>` と、**渡す型そのものがプラットフォームで違う**ので、
> `paint_gpu_surface` を 1 本にはできない。共通に保てるのは呼び出し側 —
> フレームの取り出し、bounds の計算、失敗時に CPU へ落ちる判断 — で、
> そこは実際に共通のままにしてある。「入手方法だけ」という当初の言い方は
> 実装前の見込みで、条件として満たしようがない。

> **1 つ目は実行では検証できない。ビルドは Docker で検証できる。**
> `docker run --platform linux/arm64 rust:1.95-slim` に
> `pkg-config libfontconfig1-dev libasound2-dev libx11-dev libxkbcommon-dev
> libwayland-dev libxcb1-dev libssl-dev cmake clang` を入れれば
> **`cargo check -p ravel-app` が Linux で通る**（Apple Silicon なので
> エミュレーション無しの aarch64 ネイティブ、3 分程度）。
> ただし**コンテナに GPU は無い**ので、GPU テストは
> `skipping: no GPU adapter available` で全部飛ぶ。
> 型と cfg の検証には使えるが、**リードバック 0 の証明には使えない**。
> CI の matrix は `macos-latest` と `windows-latest` だけで Linux ランナーは
> 無いが、Docker があるので**この計画で Linux のコンパイル検証を怠る理由は
> 無い**。

> **1 つ目は達成していない。理由は検証環境ではなく前提の欠落。**
> この単位の実装中に分かったことだが、**Ravel は GPUI のデバイスを採用して
> いない** — `ProjectState::new` は `GpuContext::new_blocking()` で自前の
> デバイスを作り、そのために用意された `interop::context_from_wgpu` には
> **本番の呼び出し元が 1 つも無い**。macOS はポインタ照合で同一性を確認して
> いるので安全だが、Linux には確認する手段が無く、**別デバイスのテクスチャを
> GPUI へ渡すのは未定義動作**になる。
> そこで Linux の capability は `false` に固定してある（描画側の腕は残して
> あり、配線が入れば効く）。配線は `ZC-8`。

### ZC-8 起動時に GPUI のデバイスを採用する

**`REQ-GPU-001` が要求する「UI と評価パイプラインが 1 つのデバイス」は、
macOS 以外では成立していない。** `GPUBK-9` が `interop::context_from_wgpu` を
契約として固定し、`crates/ravel-gpu/tests/device_sharing.rs` が「他人の
デバイスで抽象 API が最後まで動く」ことを機械的に確認しているが、
**アプリがそれを呼んでいない**。

`ProjectState::new` が `GpuContext::new_blocking()` で自前のデバイスを作る
現状を、ウィンドウの `gpu_context()` を採用する形へ変える。

- **評価パイプライン全体の生成方法が変わる**単位なので、`ZC-5` の射程外として
  分離した
- macOS にも影響する（今はポインタ照合で「たまたま同じ」ことを確認している
  だけで、採用しているわけではない）
- ウィンドウより先に `ProjectState` が作られる現在の順序をどうするかが要点

**完了条件**

- `interop::context_from_wgpu` に本番の呼び出し元がある
- Linux で capability が `true` になり、リードバックが 0 回
  （**Linux 実機が要る**）
- macOS が退行しない（`ZC-2`〜`ZC-4` のテストが全部通る）
- デバイス喪失・ウィンドウ再作成で破綻しない

### ZC-7 Windows の経路（`ZC-5` から分離）

**Windows でゼロコピーができないわけではない。配線が無いだけ。**
`ZC-5` の実装時に確かめた事実:

- `gpui_windows` には `gpui_wgpu` レンダラが**ある**が、非既定の `wgpu`
  feature の裏
- **既定のレンダラは D3D11**（`gpui_windows/src/directx_renderer.rs` は
  `Direct3D11::*` を使い、`ID3D11Device` / `ID3D11Texture2D` を持つ）
- **Ravel は Windows で D3D12 に乗る**（`wgpu::Backends::PRIMARY`）。
  `interop` の D3D12 対応（`NativeApi::Direct3D12`）は既にあるが、
  **相手が D3D11 なので直接は噛み合わない**
- `PlatformWindow::gpu_context` は `#[cfg(any(linux, freebsd))]` で宣言されて
  おり、**Windows には生えていない**

したがって道は 2 つ:

- **(1) `gpui_windows` の `wgpu` feature を使う。** `gpu_context` の `cfg` に
  Windows を足して実装すれば `ZC-5` / `ZC-8` の経路にそのまま乗り、
  **両者が同じ wgpu デバイスになる**ので interop 自体が要らなくなる。
  ただし**既定の D3D11 レンダラを捨てる**判断で、Windows の描画品質・性能・
  安定性が gpui-ce の非既定パスに乗る。**最も筋が良いが、影響が最も大きい**
- **(2) D3D11 と D3D12 を跨ぐ共有。** macOS の Metal interop に相当するが、
  **macOS より難しい** — 同じ API の同じデバイスではなく、**別 API 間**の
  共有になる。`IDXGIResource1` の共有ハンドル（`CreateSharedHandle` →
  `ID3D12Device::OpenSharedHandle`）を使う定石はあるが、フェンス同期も
  跨ぐ必要がある。`ZC-4` が Metal で解いた寿命問題を、より厳しい条件で
  解き直すことになる

**(1) を先に評価する。** (2) は (1) が使えないと分かった場合の退路。

**Windows 実機は利用可能**（このプロジェクトの開発者が保有）。ただし
**CI では実行検証できない**（`windows-latest` ランナーに GPU が無い）ので、
実機確認は**ブランチを push して手動で行う**運用にする。

**完了条件**

- Windows でリードバックが 0 回（**実機で確認**）
- (1) / (2) のどちらを採るかの判断が根拠付きで記録されている
- 既定レンダラを変更する場合、その影響（描画品質・性能・安定性）を
  実機で確認した結果が残っている
- `ZC-5` が置いた「テクスチャの名指し方だけが分岐する」形を壊さない

### ZC-6 文書更新と `HIGH-09` のクローズ

> **`ZC-7`（Windows）と `ZC-8`（デバイス採用）を残したまま `ZC-6` を行う。**
> `HIGH-09` の症状は開発機（macOS）の往復であり、そこは `ZC-2`〜`ZC-4` で
> 消えた。**Linux と Windows には同じ往復が残る** — Linux は配線（`ZC-8`）が、
> Windows は経路そのもの（`ZC-7`）が未了で、どちらも実機が無いと検証できない。
> 個票を閉じるときは「**macOS は解決、Linux は `ZC-8`、Windows は `ZC-7`**」と
> **残っている範囲を明記する**こと。全部解決したかのように閉じない。

- `HIGH-09` を `issues/closed/` へ（**個票が挙げた症状がすべて解決してから**。
  Windows 分は上の注記のとおり残課題として記録する）
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
