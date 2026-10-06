# [HIGH-19] タイムラインの Ctrl/Cmd + ホイールズームがルーラー原点ではなくウィンドウ座標を基準にする

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-app / Timeline |
| 該当 | `crates/ravel-app/src/panels/timeline.rs:3483-3485` |

> **解決済み**（2026-10-06）: 修正方針どおり。ホイールズームは
> `TimelineGpuiPanel::zoom_time_at` を通り、基準をスクラブ
> （`scrub_target_frame`）と同じ描画済みの `ruler_origin_x` から測る。
> 回帰テスト `a_wheel_zoom_keeps_the_frame_under_the_pointer_in_a_docked_panel`
> はパネルを 600 px ずらした状態でポインタ下のフレームが動かないことを確かめ、
> 基準を `HEADER_WIDTH` に戻す変異で落ちる（10 → 35 フレームにずれる）。

## 現状

ズームは `event.position.x - HEADER_WIDTH`（ウィンドウ空間）を使う。
一方、他のすべてのハンドラはキャプチャ済みの `ruler_origin_x` を減算する（例: `:3572` のスクラブ）。

既定の Edit プリセットではタイムラインはウィンドウ x=0 から始まらない（NodeGraph 分割の右側にある）ため、
ズームはカーソル下のフレームを保持せず内容が横方向にずれる。

## 影響

ズーム操作が「カーソル位置を維持する」という期待どおりに動かない。基本操作の体感品質を損なう。

## 修正方針

```rust
cursor_x as f64 - this.ruler_origin_x.get() as f64
```

に修正。スクラブハンドラの座標変換と同一にする。

## 検証

- 既定 Edit プリセット（タイムラインが x>0 から始まる配置）でカーソル下のフレームが
  ズーム前後で維持されるテスト

## 関連

- [medium/app-shell.md](../medium/app-shell.md) — Timeline の他のヒットテスト・座標系問題
