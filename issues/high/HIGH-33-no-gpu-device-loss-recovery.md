# [HIGH-33] GPU デバイス喪失から復帰できない — GPUI は新しいデバイスで復旧し、Ravel は死んだデバイスを持ち続ける

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-gpu / device, ravel-app / workspace |
| 該当 | `crates/ravel-app/src/workspace.rs:911-936`, `crates/ravel-app/src/project_state.rs:493-507`, `crates/ravel-gpu/src/device.rs` |

## 現状

**`ravel-gpu` にはデバイス喪失の復旧が一切無い。** `GpuContext` は起動時に一度
作られ、`Evaluator`・テクスチャプール・`GpuEvalHooks` がそれを共有したまま
セッションの最後まで生きる。デバイスが失われても、それを検出する口も、
作り直す経路も無い。

`ZC-8` が**その前提を変えた**。wgpu 経路（Linux / FreeBSD / Windows）では
`RavelWorkspace::new` がウィンドウのレンダラのデバイスを採用するので、
Ravel と GPUI が**同一の `wgpu::Device`** に乗る。ここでデバイスが失われると
両者の反応が食い違う:

- GPUI は `WgpuRenderer::recover()` で**新しい `WgpuContext` を作り直す**
  （フォーク `gpui_wgpu/src/wgpu_renderer.rs`。`WgpuContext::new_rejecting_software`
  が新しい device と queue を作る）
- Ravel は起動時に採用した**古いデバイスを持ち続ける**。評価パイプラインは
  死んだデバイス上に残り、以後フレームを 1 枚も生成できない
- 復旧が終わると `gpu_device_lost()` は再び `false` を返す。フラグは
  「いま不調か」しか答えず、「デバイスがすげ替わったか」は答えない

TDR（ドライバのリセット）やサスペンド / レジュームで現実に起きる。

## 影響

- 復帰後、GPU 評価が全滅する。CPU フォールバックも救わない
  （フォールバックは**リードバック**であり、それ自体が死んだデバイスを使う）
- ビューアが最後の GPU フレームを保持したまま再描画すると、**死んだデバイスの
  テクスチャを生きたレンダラへ渡す**経路ができる。wgpu の
  uncaptured-error ハンドラは既定でパニックするので、アプリが落ちうる

## 現在入っている緩和

`ZC-8` は 2 つ目（クロスデバイス描画）だけを塞いだ:

- `workspace.rs::host_device_unchanged()` が、採用したデバイスと**レンダラが
  いま使っているデバイス**を毎回の surface 描画前に照合する
- 食い違えば `paint_gpu_surface` が `false` を返し、既存の
  `configure_viewer_surface(false)` 経路がそのセッションの GPU surface を切る

**1 つ目（パイプラインが死ぬこと）は塞いでいない。** 落ちなくなるだけで、
復帰後に GPU 評価が動くようにはならない。

## 修正方針

デバイス喪失を `ravel-gpu` の一級の状態にする:

1. `GpuContext` に「失われた」状態を持たせ、送信側が検出できるようにする
   （wgpu の device lost コールバックは `gpui_wgpu::WgpuContext` が既に使っている）
2. `ProjectState` に再構築の経路を作る — 新しい `GpuContext` を採り、
   テクスチャプールを捨て、`Evaluator` と `GpuEvalHooks` を作り直す。
   進行中のフレームは破棄する
3. wgpu 経路では、GPUI の復旧後に `gpu_context_full()` を取り直して採用し直す。
   macOS（Metal ネイティブ）は Ravel が自前でデバイスを作るので、
   別途 `Device` の喪失検出が要る
4. headless では再現できないので、実機での手動確認手順を残す

`docs/implementation/done/zero-copy-viewer-plan.md` の `ZC-4` / `ZC-8` の完了条件
「デバイス喪失・ウィンドウ再作成で破綻しない」は**この項目が閉じるまで
満たされない**。

## 現在地（2026-09-03）

**修正方針の 1〜3 は実装済みで、`done/gpu-device-loss-recovery-plan.md` の
`GPULOSS-1`〜`GPULOSS-5` が全部マージされている**（#485 / #493 / #495 / #500）。
device state（epoch + lost）、評価 worker の epoch 交換、採用 device の
ポーリングと再採用、macOS の安全側確定、pool lease と window lifecycle の
後片付けまで入り、いずれも自動テストと変異注入で固定してある。

**それでもこの項目を閉じないのは 4 が残っているため** — 実機の device loss で
復帰するところを**誰も見ていない**。macOS には意図的に device を失わせる手段が
無く、確認には Windows 実機（`Win+Ctrl+Shift+B` のドライバ再起動）が要る。
手順は計画書の `GPULOSS-5` 節にある。

派生して開いている項目: `MED-APP-40`（macOS は GPUI の Metal device の喪失を
問う口を持たない）、`MED-APP-41`（zero-copy の可否が session 全体で 1 個）、
`MED-APP-42`（自前 device 喪失では退役フレームが残りうる）。

