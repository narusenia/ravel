# [HIGH-09] ビューア画像経路が毎フレーム GPU→CPU→GPU の往復 + 冗長コピー + アトラス churn

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-gpu / frame, ravel-app / Viewer |
| 該当 | `crates/ravel-gpu/src/frame.rs:134-143`, `crates/ravel-app/src/eval_hooks.rs:121-130`, `crates/ravel-ui/src/panels/viewer.rs`（`ViewerResolution`）, `crates/ravel-app/src/panels/viewer.rs:283-288`, `:1599-1604` |

> **macOS では往復が消えた。Linux / Windows には残っている（2026-08-12 時点）。**
> 個票が挙げた 4 つの症状のうち 3 つは以前から片付いており、4 つ目（往復）が
> プラットフォームで状態が分かれる。**この個票が open なのは 4 つ目のため。**
>
> | 症状 | 状態 |
> |---|---|
> | UI スレッドでの f32 → BGRA 変換 | ✅ `GPUCOMP-9`（#284）が評価ワーカーへ移した（`HIGH-08` 解決）。さらに #363 で **10.1× 高速化**（rayon + 境界表、`perf-baseline.md`） |
> | リードバック実装そのもの | ✅ `GPUCOMP-8` を `GPUBK-6`（#282）が回収 |
> | 解像度上限（`VIEWER_MAX_DIM`） | ✅ `VRES-1`（#300）が定数を撤去し係数モデルへ |
> | **GPU→CPU→GPU の往復** | **macOS ✅ / Linux ❌ / Windows ❌**（下記） |
>
> ### 往復のプラットフォーム別状態
>
> | | 状態 | 引受先 |
> |---|---|---|
> | **macOS** | ✅ **解決**（#382 / #384 / #386） | — |
> | **Linux** | ❌ 描画側はあるが**既定オフ** | `ZC-8`（デバイス採用の配線） |
> | **Windows** | ❌ 未着手 | `ZC-7`（既定 D3D11 レンダラをどうするか） |
>
> 引受計画:
> [`zero-copy-viewer-plan.md`](../../docs/implementation/zero-copy-viewer-plan.md)。
> `ZC-1`（#373）が判断ゲートを開け、`ZC-2`（#382）が GPUI の Metal デバイスを
> 取り込み、`ZC-3`（#384）が surface 経路を通し、`ZC-4`（#386）が寿命を閉じて
> 既定を有効にした。`ZC-5`（#388）は Linux の描画側を置いたが、
> **`interop::context_from_wgpu` に本番の呼び出し元が無い**ことが分かったため
> 既定オフのまま。macOS が動いているのはポインタ照合で同一デバイスを
> 確認しているからで、採用しているわけではない。
>
> **`MED-GPU-07`（`Cargo.lock` に wgpu が 2 本）は前提ではない — 2026-08-05 に
> 解決済み**（この個票が以前そう書いていたのは誤り）。
>
> **クローズは `ZC-7` / `ZC-8` が済んでから。** 往復が残っている
> プラットフォームがある間は open のままにする。

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

- ~~短期: `to_frame_buffer` の2度目のコピーを除去し、変換バッファを永続化・再利用~~
  → `GPUBK-6`（#282）が回収済み
- 本質: フレームを GPU 常駐のまま共有テクスチャ / カスタム GPUI 要素で表示し、
  フレームごとのリードバックとアトラス再アップロードを廃止
  → **macOS は完了**（`ZC-2`〜`ZC-4`）。残りは `ZC-7` / `ZC-8`

## 検証

**観測量は時間ではなく回数。** 時間は負荷で動くが回数は動かない
（`zero-copy-viewer-plan.md` の検証節）。

- `crates/ravel-nodes/tests/display_surface.rs` の
  `the_surface_path_removes_the_readback_and_the_fallback_keeps_it` が、
  ゼロコピー経路で `readbacks == 0`、フォールバックで `== 1` を機械的に固定
  している。**0 と 0 では退行を検出できない**ので両方を見る
- 絵の一致は同ファイルの `both_roads_produce_the_same_display_bytes`
  （GPU / CPU 両経路のバイト列が完全一致）
- 実機では平面レイヤーを 299 フレーム再生してティアリングが無いことを確認
  （`ZC-4`、#386）
- **時間の実測は載せていない。** 消える段のうち「GPUI のアップロードと
  アトラス churn」は gpui-ce の内部にありウィンドウ無しには測れず、
  総和を主張すると測れない部分を含んだ数字になる。`ZC-1` の推定
  （`perf-baseline.md`）が上限を示している

## 関連

- [HIGH-04](../closed/HIGH-04-per-frame-blocking-readback.md), [HIGH-05](../closed/HIGH-05-shell-chain-cpu-per-pixel.md), [HIGH-08](../closed/HIGH-08-ui-thread-f32-to-bgra-conversion.md)
