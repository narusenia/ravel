# medium — UI レンダリング（ravel-app パネル）

[CRIT-01](../critical/CRIT-01-eval-update-notifies-whole-workspace.md) と
[HIGH-07](../high/HIGH-07-document-changed-cascade-per-mouse-move.md) の修正で
呼ばれる**回数**は減るが、1回あたりのコストは以下で個別に残る。

---

## MED-UI-01 | perf | 編集ごとに UI スレッドでコンポジションを再コンパイル（純粋なパラメータ変更でも）

**該当**: `crates/ravel-app/src/project_state.rs:763`（`document_changed` 内の `self.compiled = None`）、
`:860-877`（`compiled_root` → `compile_composition`）

`document_changed` は `InvalidationHint::Params` でもコンパイル済みシェルチェーンを
無条件に破棄する。次の `request_viewer_eval`（同じ UI スレッドの呼び出しスタック、
スクラブティックごと）がアクティブコンポジションを再コンパイルする。
コストはレイヤー数に比例し、任意のジェスチャー中はマウス移動ごとに走る。

**修正方針**: ヒントが `Params` のときはコンパイル済みチェーンを保持する
（構造は不変。コンパイラは既に決定的 ID を使っている）。
`Structural` ヒントとコンポジション切替時のみ再構築。

---

## MED-UI-02 | perf | Properties パネルが再生中フレームあたり2回、全セクションを再構築する

**該当**: `crates/ravel-app/src/panels/properties.rs:579-597`（project observer と
PlaybackPosition observer）、`:1220-1255`（`refresh_values`）

再生中は `PlaybackPosition` observer（`publish_position` ごと、`playback.rs:417`）と
ProjectState observer（[CRIT-01](../critical/CRIT-01-eval-update-notifies-whole-workspace.md)
経由で評価結果ごと）の**両方**が `refresh_values` を呼ぶ。
`refresh_values` はドキュメントからターゲットを再解決し、全アニメーションチャンネルを再サンプルし、
新しい文字列を伴う全 `PropertySection` を再構築して notify する — 30〜60fps で毎フレーム2回。

**修正方針**: 2つのトリガーを重複排除（フレームあたり最大1回）。
アニメーションチャンネル由来のフィールドのみ再構築。パネル非表示時はスキップ。

---

## MED-UI-03 | perf | Timeline に行の仮想化が無い — 全レイヤーのヘッダとレーンを毎フレーム構築・描画

**該当**: `crates/ravel-app/src/panels/timeline.rs:3022-3057`（`build_layer_headers`）、
`:2586-2723`（レイヤー領域の描画ループ）、`:2535-2549`（`total_layer_height`）

`build_layer_headers` はスクロールビューポートに関係なく**全**レイヤーの div/button サブツリーを
構築し、`keyframes::property_rows` を計算する。
レイヤー領域のキャンバスループはレーン境界・バー・キーフレームを全レイヤー分描画する
（バーは x 方向にカリングされるが、可視スクロール範囲に対する**垂直カリングは無い**）。
レイヤー数に線形にスケールする。

**修正方針**: ヘッダビルダーと キャンバス描画の両方で可視 y 範囲にカリングする
（行レイアウトは既に算術的 — `row_at_content_y` にその計算がある）。
またはヘッダを `uniform_list` に移す。

---

## MED-UI-04 | perf | Timeline が Composition を deep compare し、レンダーごとにパネル状態を約5回 clone する

**該当**: `crates/ravel-app/src/panels/timeline.rs:354-401`（`sync_from_project`）、
`:2385`, `:2557`, `:2773`, `:2793`, `:3432`（状態 clone）

`sync_from_project` は ProjectState notify ごとに走り、
`self.state.composition() != comp.as_ref()` — 全レイヤー / 全ネットワークにわたる
深い等価判定を実行し、**何も変わっていなくても** `cx.notify()` を呼ぶ。
各レンダーは `self.state`（Composition を内包）をルーラー・レイヤー領域・カーブグリッド・
カーブシェル・コンテキストメニュー用に clone する。
`im` の構造共有で clone 自体は比較的安いが、notify ごとの比較 + 確定再レンダーは無駄。

**修正方針**: deep equality ではなく ProjectState が既に持つ `revision` カウンタを比較。
ミラーしたコンポジションまたは選択が実際に変化したときのみ notify。

---

## MED-UI-05 | perf | Outliner と MediaBin が ProjectState notify ごとに全行モデルを再構築する

**該当**: `crates/ravel-app/src/panels/outliner.rs:97`, `:146-168`,
`crates/ravel-app/src/panels/media_bin.rs:78`,
`crates/ravel-ui/src/panels/outliner.rs:181-208`

`rebuild_rows` は全コンポジション・全レイヤー・全ネットワークノードを走査し、
行ごとにラベル文字列を確保して notify する。
ドキュメント変更ごと（ドラッグティックごと）に、さらに
[CRIT-01](../critical/CRIT-01-eval-update-notifies-whole-workspace.md) 経由で
再生中の評価結果ごとにも走る。

**修正方針**: 再構築をドキュメントリビジョンチェックでゲート。
評価更新経路からこれらのパネルへ notify しないようにする
（CRIT-01 の修正で大部分は解消）。

---

## MED-UI-06 | perf | 同じ変更が2経路から届き、パネルが同じ再解決を2回走らせる

**該当**: `crates/ravel-app/src/panels/properties.rs:552-573`（`SelectedPropertiesTarget`
observer）と `:579-597`（project observer）、
`crates/ravel-app/src/panels/timeline.rs:302-310` / `outliner.rs:107-118`
（`ActiveComposition` observer）

1つの変更が「グローバル書き込み」と「`ProjectState` notify」の両方として届く箇所がある。

- ノードパラメータのドラッグ: NodeEditor の `refresh_from_document` が
  `notify_properties_selection` を呼ぶ（選択が非空のとき）→ Properties の
  target observer が `refresh_values_checked`。同じ move の project notify でも
  もう一度 `refresh_values_checked`。**move ごとに全セクション2回再解決**
- コンポジション切替: Timeline / Outliner は `ActiveComposition` observer で
  sync し、同じ切替の project notify でもう一度 sync する（deep compare と
  全行走査が2回）

グローバル駆動の sync は生きたドキュメントから読み直すので、その epoch は
既にカバーされている。[HIGH-07](../high/HIGH-07-document-changed-cascade-per-mouse-move.md)
の epoch ゲート（`panels::MirrorEpoch`）に「グローバル駆動の sync 後に
現在の epoch を記録する」を足せば、対になる notify を吸収できる。

**未着手の理由**: GPUI は1エフェクトサイクル内の `cx.notify()` を合流させるため、
observer 数を数えるプローブでは削減も回帰も観測できない。sync 関数の呼び出し回数を
数える計装（`tracing` span カウンタ等）を先に用意しないと、
入れた・戻したの判断が測定に基づかない。ドラッグ経路の方が影響が大きい。

---

## 参考: 監査で問題なしと確認された箇所

- 評価ワーカーの latest-wins 合流（`eval_service.rs:157-181`）は健全。スクラブ中の評価バックログを防いでいる
- 再生ティックループはイベント駆動。ravel-app にアイドルポーリングタイマーは無い
- `frame_buffer_to_render_image` は `render()` の外に正しく置かれている
- `RenderImage` のアトラスリークは `drop_image` で正しく処理されている
- カーブエディタにはサンプル予算がある
- Properties のウィジェット再構築は `needs_rebuild` でゲートされている
