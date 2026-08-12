# closed / medium — UI レンダリング（ravel-app パネル）

解決済みの medium 項目。個票は起票時のまま残し、各項目の **解決済み** 行が結果を記録している。

未解決分は [`../medium/ui-rendering.md`](../medium/ui-rendering.md)。

---

## MED-UI-01 | perf | 編集ごとに UI スレッドでコンポジションを再コンパイル（純粋なパラメータ変更でも）

**該当**: `crates/ravel-app/src/project_state.rs:763`（`document_changed` 内の `self.compiled = None`）、
`:860-877`（`compiled_root` → `compile_composition`）

> **解決済み**: `RESP3-6`（PR #397）。`document_changed` は
> `InvalidationHint::Structural` のときだけコンパイル済みシェルチェーンを破棄する。
> `Params` / `None` では保持する — チェーンが焼き込むのは**形**だけで、値は
> リクエストが運ぶ Document から process 時に読まれるので、パラメータ編集は
> チェーンを作り直さなくてもビューアに出る。

`document_changed` は `InvalidationHint::Params` でもコンパイル済みシェルチェーンを
無条件に破棄する。次の `request_viewer_eval`（同じ UI スレッドの呼び出しスタック、
スクラブティックごと）がアクティブコンポジションを再コンパイルする。
コストはレイヤー数に比例し、任意のジェスチャー中はマウス移動ごとに走る。

**修正方針**: ヒントが `Params` のときはコンパイル済みチェーンを保持する
（構造は不変。コンパイラは既に決定的 ID を使っている）。
`Structural` ヒントとコンポジション切替時のみ再構築。

---

## MED-UI-03 | perf | Timeline に行の仮想化が無い — 全レイヤーのヘッダとレーンを毎フレーム構築・描画

**該当**: `crates/ravel-app/src/panels/timeline.rs:3022-3057`（`build_layer_headers`）、
`:2586-2723`（レイヤー領域の描画ループ）、`:2535-2549`（`total_layer_height`）

> **解決済み**: `RESP3-8`（PR #397）。ヘッダ構築は `ScrollHandle` の
> オフセットから、キャンバス描画は `window.content_mask()` から可視 y 範囲を
> 取り、その範囲の行だけを構築・描画する。行レイアウトの算術は
> `row_at_content_y` 側を再利用している。
>
> **票の「`keyframes::property_rows` を全レイヤー分計算する」は実測では
> 起きていなかった** — 折り畳まれたレイヤーでは行モデル構築が元から走らない
> （100 枚折り畳みで 0 回）。カリングで消えたのはヘッダのウィジェット構築と
> キャンバス描画の方。

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

> **解決済み**: `RESP3-9`（PR #397）。`sync_from_project` の deep compare を
> `ProjectState.revision` + `MirrorEpoch` のゲートに置き換えた。revision が
> 進んでいなければ sync も `cx.notify()` も走らない。
>
> **レンダーごとの `self.state` clone 5 箇所は残っている。** `move`
> クロージャが所有権を要求するので参照にできず、`im` の構造共有で
> clone 自体は安い（票も「clone 自体は比較的安い」と書いている）。
> 消えたのは notify ごとの deep compare と確定再レンダーの方。

`sync_from_project` は ProjectState notify ごとに走り、
`self.state.composition() != comp.as_ref()` — 全レイヤー / 全ネットワークにわたる
深い等価判定を実行し、**何も変わっていなくても** `cx.notify()` を呼ぶ。
各レンダーは `self.state`（Composition を内包）をルーラー・レイヤー領域・カーブグリッド・
カーブシェル・コンテキストメニュー用に clone する。
`im` の構造共有で clone 自体は比較的安いが、notify ごとの比較 + 確定再レンダーは無駄。

**修正方針**: deep equality ではなく ProjectState が既に持つ `revision` カウンタを比較。
ミラーしたコンポジションまたは選択が実際に変化したときのみ notify。

---

## MED-UI-06 | perf | 同じ変更が2経路から届き、パネルが同じ再解決を2回走らせる

**該当**: `crates/ravel-app/src/panels/properties.rs:552-573`（`SelectedPropertiesTarget`
observer）と `:579-597`（project observer）、
`crates/ravel-app/src/panels/timeline.rs:302-310` / `outliner.rs:107-118`
（`ActiveComposition` observer）

> **解決済み**: `RESP3-11`（PR #397）。グローバル駆動の sync のあとに現在の
> epoch を記録し、対になる project notify がそれを見て sync を飛ばす。
> `RESP3-5` の計装で、ドラッグ 1 move あたりの Properties 再解決は
> **16 → 10 回（= 1 回 / move）**、コンポジション切替の Timeline / Outliner は
> **2 → 1 回**。
>
> **Outliner は `sync_tree()` に集約した** — `ActiveComposition` observer と
> project observer のどちらが先に走っても、そこで採用が済む形にしてある。

1つの変更が「グローバル書き込み」と「`ProjectState` notify」の両方として届く箇所がある。

- ノードパラメータのドラッグ: NodeEditor の `refresh_from_document` が
  `notify_properties_selection` を呼ぶ（選択が非空のとき）→ Properties の
  target observer が `refresh_values_checked`。同じ move の project notify でも
  もう一度 `refresh_values_checked`。**move ごとに全セクション2回再解決**
- コンポジション切替: Timeline / Outliner は `ActiveComposition` observer で
  sync し、同じ切替の project notify でもう一度 sync する（deep compare と
  全行走査が2回）

グローバル駆動の sync は生きたドキュメントから読み直すので、その epoch は
既にカバーされている。[HIGH-07](HIGH-07-document-changed-cascade-per-mouse-move.md)
の epoch ゲート（`panels::MirrorEpoch`）に「グローバル駆動の sync 後に
現在の epoch を記録する」を足せば、対になる notify を吸収できる。

**未着手の理由**: GPUI は1エフェクトサイクル内の `cx.notify()` を合流させるため、
observer 数を数えるプローブでは削減も回帰も観測できない。sync 関数の呼び出し回数を
数える計装（`tracing` span カウンタ等）を先に用意しないと、
入れた・戻したの判断が測定に基づかない。ドラッグ経路の方が影響が大きい。
