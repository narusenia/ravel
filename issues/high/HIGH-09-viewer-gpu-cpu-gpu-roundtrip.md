# [HIGH-09] ビューア画像経路が毎フレーム GPU→CPU→GPU の往復 + 冗長コピー + アトラス churn

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-gpu / frame, ravel-app / Viewer |
| 該当 | `crates/ravel-gpu/src/frame.rs:134-143`, `crates/ravel-app/src/eval_hooks.rs:121-130`, `crates/ravel-ui/src/panels/viewer.rs`（`ViewerResolution`）, `crates/ravel-app/src/panels/viewer.rs:283-288`, `:1599-1604` |

> **残っているのはゼロコピー表示だけ（2026-08-10 時点）。** この個票が挙げた
> 4 つの症状のうち 3 つは片付いている。
>
> | 症状 | 状態 |
> |---|---|
> | UI スレッドでの f32 → BGRA 変換 | ✅ `GPUCOMP-9`（#284）が評価ワーカーへ移した（`HIGH-08` 解決）。さらに #363 で **10.1× 高速化**（rayon + 境界表、`perf-baseline.md`） |
> | リードバック実装そのもの | ✅ `GPUCOMP-8` を `GPUBK-6`（#282）が回収 |
> | 解像度上限（`VIEWER_MAX_DIM`） | ✅ `VRES-1`（#300）が定数を撤去し係数モデルへ |
> | **GPU→CPU→GPU の往復そのもの** | ❌ **未実装。引受先の計画が無い** |
>
> `GPUBK-9`（#296）は**判断の単位**で、`gpu-backend-plan.md` の非対象節が
> 「ゼロコピー表示の実装。`GPUBK-9` で判断し、必要なら別計画」と書いている。
> **その別計画を 2026-08-10 に書いた**:
> [`zero-copy-viewer-plan.md`](../../docs/implementation/zero-copy-viewer-plan.md)。
>
> **`MED-GPU-07`（`Cargo.lock` に wgpu が 2 本）は前提ではない — 2026-08-05 に
> 解決済み**（この個票が以前そう書いていたのは誤り）。Ravel は他人のデバイスを
> 受け取れる。残る穴は GPUI 側の 2 つで、(1) gpui がレンダラのデバイスを
> 公開していない、(2) **macOS の gpui は wgpu ではなく Metal ネイティブ**なので
> 共有すべき `wgpu::Device` が存在しない。計画は Metal レベルの interop から
> 始める。
>
> **着手の判断は #367 後の数字で測り直すこと**（`ZC-1` がその単位）。
> CPU 変換が 38ms → 3.7ms に
> なったので、往復を消して得られる残りの利得は個票を書いた時点より小さい。

## 現状

GPU 評価されたフレームは毎回

1. `read_texture` で CPU f32 にリードバック
2. `bytemuck::cast_slice(&raw).to_vec()` で**2度目のコピー**
   （1024×576 RGBA-f32 で約 9.4MB）
3. UI 側で新規 `RenderImage` として GPUI スプライトアトラスへ再アップロード、
   前フレームの画像を `drop_image`（アトラス churn）

## 影響

ビューアのスループット上限を決めている。`VRES-1` 以前は対話評価が
`VIEWER_MAX_DIM = 1024`（長辺の絶対上限）に制限されており、その理由が
この経路のコストだった。`VRES-1` で上限は撤去され、ユーザーが選ぶ
`ViewerResolution` 係数（既定 `Half`）に置き換わっている。**この経路の
コストは変わっていない** — 変わったのは、ユーザーが `Full` を選んで
コンポ解像度で評価できるようになり、そのとき本 issue のコストを
まともに踏むこと。`GPUBK-9` の計測（`perf-baseline.md`）では 1080p 全体
約 15.8 ms のうち転送 2.04 ms + BGRA 変換 1.63 ms が本 issue の範囲で、
支配項ではない（約 23%）。

## 修正方針

- 短期: `to_frame_buffer` の2度目のコピーを除去し、変換バッファを永続化・再利用
- 本質（実装計画の Phase 4）: フレームを GPU 常駐のまま共有テクスチャ / カスタム GPUI 要素で表示し、
  フレームごとのリードバックとアトラス再アップロードを廃止

## 検証

- フレームあたりのバイトコピー量を計測（現状の半減が短期目標）
- `ViewerResolution::Full` でも再生レートが維持できることを確認

## 関連

- [HIGH-04](../closed/HIGH-04-per-frame-blocking-readback.md), [HIGH-05](../closed/HIGH-05-shell-chain-cpu-per-pixel.md), [HIGH-08](../closed/HIGH-08-ui-thread-f32-to-bgra-conversion.md)
