# medium — UI レンダリング（ravel-app パネル）

[CRIT-01](../closed/CRIT-01-eval-update-notifies-whole-workspace.md) と
[HIGH-07](../closed/HIGH-07-document-changed-cascade-per-mouse-move.md) の修正で
呼ばれる**回数**は減るが、1回あたりのコストは以下で個別に残る。

---

## MED-UI-02 | perf | Properties パネルが再生中フレームあたり2回、全セクションを再構築する

**該当**: `crates/ravel-app/src/panels/properties.rs:579-597`（project observer と
PlaybackPosition observer）、`:1220-1255`（`refresh_values`）

再生中は `PlaybackPosition` observer（`publish_position` ごと、`playback.rs:417`）と
ProjectState observer（[CRIT-01](../closed/CRIT-01-eval-update-notifies-whole-workspace.md)
経由で評価結果ごと）の**両方**が `refresh_values` を呼ぶ。
`refresh_values` はドキュメントからターゲットを再解決し、全アニメーションチャンネルを再サンプルし、
新しい文字列を伴う全 `PropertySection` を再構築して notify する — 30〜60fps で毎フレーム2回。

**修正方針**: 2つのトリガーを重複排除（フレームあたり最大1回）。
アニメーションチャンネル由来のフィールドのみ再構築。パネル非表示時はスキップ。

> **部分的に解決（`RESP3-7`、PR #397）**: 「プレイヘッドで再サンプルされるものが
> 何も無いときスキップ」が入り、静的ターゲットの再生 1 秒あたりの
> `refresh_values` は 30 → 1 回になった。アニメーション有りのターゲットは
> 30 回のまま据え置きで、これは正しい（削ると値が止まる）。
>
> **残っているのは「パネル非表示のときスキップ」。** タブの可視性がパネルへ
> 届く仕組みが無く（`ravel-dock` は `active: usize` を持つがパネルへ伝えない）、
> 配線は設計ゲート規模になる。この 1 点が未達なので本項目は未解決のまま置く。

---

## MED-UI-05 | perf | Outliner と MediaBin が ProjectState notify ごとに全行モデルを再構築する

**該当**: `crates/ravel-app/src/panels/outliner.rs:97`, `:146-168`,
`crates/ravel-app/src/panels/media_bin.rs:78`,
`crates/ravel-ui/src/panels/outliner.rs:181-208`

`rebuild_rows` は全コンポジション・全レイヤー・全ネットワークノードを走査し、
行ごとにラベル文字列を確保して notify する。
ドキュメント変更ごと（ドラッグティックごと）に、さらに
[CRIT-01](../closed/CRIT-01-eval-update-notifies-whole-workspace.md) 経由で
再生中の評価結果ごとにも走る。

**修正方針**: 再構築をドキュメントリビジョンチェックでゲート。
評価更新経路からこれらのパネルへ notify しないようにする
（CRIT-01 の修正で大部分は解消）。

> **部分的に解決（`RESP3-10`、PR #397）**: **Outliner 側は解決した。**
> `push_layer_rows` が `expandable` を決めるために折り畳まれたレイヤーまで
> `network_rows` を走らせていたのを `network_has_rows` に置き換え、
> 割り当てゼロ・走査 1 回にした。行が前回と同一なら `cx.notify()` もしない。
>
> **残っているのは MediaBin** — ドラッグ 1 move ごとに 10 回再構築する。
> 早期 return を入れると `HIGH-07` の既存退行テスト 2 本
> （`a_completed_save_rebuilds_no_document_panel` /
> `a_composition_switch_leaves_every_gate_open_for_the_next_edit`）が落ちる。
> 両テストは「ドキュメント編集ではすべてのミラーパネルが notify する」と
> 主張しているが、MediaBin についてはその前提の方が誤り（レイヤー編集は
> media 資産を変えない）。**退行テストを緩める判断が要るので保留。**

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
