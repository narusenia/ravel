# medium — UI レンダリング（ravel-app パネル）

起票分はすべて解決した（[`../closed/medium-ui-rendering.md`](../closed/medium-ui-rendering.md)）。
このファイルに残るのは、監査で問題なしと確認した箇所の記録だけ。

---

## 参考: 監査で問題なしと確認された箇所

- 評価ワーカーの latest-wins 合流（`eval_service.rs:157-181`）は健全。スクラブ中の評価バックログを防いでいる
- 再生ティックループはイベント駆動。ravel-app にアイドルポーリングタイマーは無い
- `frame_buffer_to_render_image` は `render()` の外に正しく置かれている
- `RenderImage` のアトラスリークは `drop_image` で正しく処理されている
- カーブエディタにはサンプル予算がある
- Properties のウィジェット再構築は `needs_rebuild` でゲートされている
