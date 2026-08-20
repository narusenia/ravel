# medium — UI レンダリング（ravel-app パネル）

[CRIT-01](../closed/CRIT-01-eval-update-notifies-whole-workspace.md) と
[HIGH-07](../closed/HIGH-07-document-changed-cascade-per-mouse-move.md) の修正で
呼ばれる**回数**は減るが、1回あたりのコストは以下で個別に残る。

---

## MED-UI-07 | bug | 狭い Properties パネルで Vector 行が横にはみ出して見えなくなる

**該当**: `crates/ravel-app/src/panels/properties.rs` の `PropertyField::Vector`
分岐（成分ごとのスクラブ入力セル）

Vector 行は成分ごとに `min_w(px(56.0))` のセルを横並びにする。この最小幅が
flex item の自動最小幅を固定するため、行の値セルはパネル幅より狭くなれない。
パネルを狭めると値セルが右へはみ出し、ルートは `overflow_y_scroll` のみで
横スクロールを持たないので、はみ出した成分は**到達不能**になる。

実測（パネル幅 160px、3成分 Vector、`VisualTestContext` の
`debug_bounds` でセル境界を採寸）:

| 状態 | セル幅 | セル右端 | パネル右端 |
|---|---|---|---|
| PR #214 以前 | 176px | 255px | 160px |
| PR #214 以後 | 176px | 211px | 160px |

PR #214（ラベルの `min_w_0().truncate()` 化）でラベルが場所を譲るようになり
はみ出しは 44px 縮んだが、セル幅 176px 自体は変わらないので解消はしていない。
4成分（`Channel4` 由来）ではさらに 56px + gap ぶん悪化する。

**修正方針**: 値セルを折り返し可能にする（`flex_wrap` + セルの
`min_w` 引き下げ）か、狭幅ではラベルと値を縦積みにする
（Properties の `String` 分岐が既に採っている形）。いずれもパネル幅の
しきい値が要るので、`Vector` 分岐だけの局所修正では収まらない。
併せて狭幅での境界テストを追加する。

---

## 参考: 監査で問題なしと確認された箇所

- 評価ワーカーの latest-wins 合流（`eval_service.rs:157-181`）は健全。スクラブ中の評価バックログを防いでいる
- 再生ティックループはイベント駆動。ravel-app にアイドルポーリングタイマーは無い
- `frame_buffer_to_render_image` は `render()` の外に正しく置かれている
- `RenderImage` のアトラスリークは `drop_image` で正しく処理されている
- カーブエディタにはサンプル予算がある
- Properties のウィジェット再構築は `needs_rebuild` でゲートされている
