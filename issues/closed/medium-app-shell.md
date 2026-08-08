# closed / medium — ravel-app / ravel-ui（シェル・パネル・状態管理）

解決済みの medium 項目。個票は起票時のまま残し、各項目の **解決済み** 行が結果を記録している。

未解決分は [`../medium/app-shell.md`](../medium/app-shell.md)。

---

## MED-APP-01 | bug | 分離パネルの OS ウィンドウを閉じるとシェルが desync、`reattach_window` は dead API

> **解決済み**: PR #236（2026-08-01）。全ウィンドウ同型モデルで**故障モード自体が
> 消えた**。分離ウィンドウは全て `on_window_should_close` を登録し、クローズは
> 必ず `AppShell::close_window` を通る（＝レイアウトからの窓削除 + インスタンス
> 破棄）。論理 `WindowId` ↔ GPUI ハンドルの表は `WindowRegistry` 1 つに集約され、
> `DetachedWindowHandles` と `reattach_window` は削除した。多重インスタンス化に
> よりクローズは非可逆でも喪失にならない（同じパネルは View トグルで出し直せる）。
> 統合テスト: `tests/detached_window_host.rs`。設計は
> `docs/implementation/done/free-pane-docking-plan.md` の `DOCK-6`。

**該当**: `crates/ravel-app/src/workspace.rs:566-605`, `crates/ravel-ui/src/shell.rs:132-145`

`AppShell::reattach_window` は「分離 OS ウィンドウがユーザーに閉じられたときホストが呼ぶ」と
文書化されているが呼び出し元がゼロ。`open_detached` はクローズハンドラを登録しない。

タイトルバーで分離ウィンドウを閉じると、そのパネルはどこにも表示されなくなり
（メインドック内では hidden、ウィンドウは消滅）、`DetachedWindowHandles` に stale ハンドルが残り、
シェルは分離状態のままになる。復帰手段は Cmd+Shift+R（「最後に分離したパネル」へのフォールバック）だけ。

**修正方針**: 分離ウィンドウに `on_window_should_close` を登録し、
`shell.reattach_window(id)` を呼んでパネルをドックへ復帰させる。

**引受先**: `docs/implementation/done/free-pane-docking-plan.md` の `DOCK-6`。
全ウィンドウ同型モデルでは分離窓クローズ = インスタンス破棄となり、
「シングルトンの行方不明」という故障モード自体が消える。現行系への
先行修正はしない（計画の決定事項）。

---

## MED-APP-12 | bug | GPU コンテキスト初期化が起動時に panic、エラーダイアログ無し

**該当**: `crates/ravel-app/src/project_state.rs:184`

> **解決済み**: フェーズ A2。`ProjectEvent::GpuInitializationFailed { error }` が
> 起動時の初期化失敗を通知に落とす（`crates/ravel-app/src/project_state.rs:96-98`）。

`GpuContext::new_blocking().expect("GPU context initialization failed")` —
wgpu がアダプタを得られないマシン / ドライバでは毎回の起動でクラッシュする。
同ファイルのメインウィンドウ失敗経路（`main.rs:101-105` はログ出力して正常終了）と不整合。

**修正方針**: エラーを伝播させて致命的エラーダイアログを表示する
（またはウィンドウ経路と同様に log-and-quit）。

---

## MED-APP-16 | bug | 資産由来のキーバインドが context なしで登録され、テキスト入力から矢印キーを奪う

**該当**: `crates/ravel-app/src/workspace.rs:256`, `assets/keybindings/default.toml:45-46`

> **解決済み**: フェーズ A。資産由来のバインドはすべて `Some("!Input")` context で
> 登録され、フォーカス中のテキスト入力が矢印・編集・クリップボードを保持する
> （`crates/ravel-app/src/workspace.rs:254-259`）。

キーバインド資産から読んだ**全バインドが context `None`（グローバル）**で登録される。

```rust
// workspace.rs:256
out.push(KeyBinding::new(&gpui_chord, $Action, None));
```

`default.toml:45-46` は `step_forward = "Right"` / `step_backward = "Left"`。
context なしのバインドはあらゆる context でマッチするため、テキスト入力に
フォーカスがある状態でも矢印がアクションに食われる。

gpui-component の Input は `Left` / `Right` を**アクションとして処理している**
（`InputState::left` / `right` を `on_action` で登録。バインドは `Some("Input")`
context 付き）。両方がマッチするので、どちらが勝つかは登録順に依存する
不安定な状態になっている。

同じファイルのパネル固有バインド（`workspace.rs:269-284`）は
`Some(panels::node_editor::KEY_CONTEXT)` などを正しく渡しており、
**資産由来のバインドだけが穴**。

**修正方針**: 資産に context 欄を追加し、矢印・単独英字のような単一キー系を
パネル context か否定述語に閉じる。GPUI の context predicate は `!` / `&&` /
`||` / `>` を解釈するので、Input の key context（`"Input"`）に対して
`Some("!Input")` が書ける。

**検証**: テキスト入力にフォーカスがある状態で `Right` を押してもフレームが
進まず、キャレットが動くテスト。フォーカスが無い状態ではフレームが進むテスト。

---

## MED-APP-18 | bug | ScrubInput のテキスト編集が全選択で始まらない（`defer_in` のタイミングで dispatch が捨てられる）

**該当**: `crates/ravel-app/src/widgets/scrub_input.rs:221-232`

> **解決済み**: フェーズ A。`begin_edit` が `SelectAll` の dispatch をやめ、
> `state.set_selected_range(0..text_len, cx)` を直接呼ぶ。回帰テストは
> `text_edit_starts_with_the_value_selected`
> （`crates/ravel-app/src/widgets/scrub_input.rs:221-230`, `:490-503`）。

クリックでテキスト編集に入るとき、値を全選択して打ち始めれば置き換わるように
`SelectAll` を dispatch している。

```rust
// scrub_input.rs:226-232
let editor = cx.new(|cx| InputState::new(window, cx).default_value(text));
editor.update(cx, |state, cx| state.focus(window, cx));
// Select the whole value so typing replaces it (AE behavior). The
// action must dispatch after the Input has rendered into the tree.
cx.defer_in(window, |_this, window, cx| {
    window.dispatch_action(Box::new(gpui_component::input::SelectAll), cx);
});
```

`SelectAll` の受け側は存在する。gpui-component の Input はルート div に
`key_context("Input")` と `track_focus(state.focus_handle)` を張り、
`on_action(window.listener_for(&self.state, InputState::select_all))` を
登録している。

**問題は dispatch のタイミング**。`cx.defer_in` は現在のエフェクトサイクル末尾で
流れるため、その時点で新規作成した Input の div はまだ dispatch ツリーに
入っていない（次の render で入る）。`window.dispatch_action` はハンドラを
見つけられず**黙って捨てられる**。コメントの意図（"after the Input has
rendered"）は正しいが、`defer_in` はそれを保証しない。

**修正方針**: `window.on_next_frame` で dispatch すれば 1 フレーム後になるが、
打ち始めが速いと取りこぼすため再発する。gpui-component は narusenia の fork
（`Cargo.toml:33-34`）なので、**上流に公開の `select_all` を足して直接呼ぶ**のが
確実（`InputState::select_all` は現在 `pub(super)`、公開されている
`set_value` / `selected_range` では選択範囲を設定できない）。

**検証**: 編集に入った直後の `selected_range()` が値全体になるテスト。

---

## MED-APP-22 | bug | `Cmd+Shift+D` の直後の `Cmd+Shift+R` が空振りする

> **解決済み**: PR #247（2026-08-01）。分離窓は「あるペインの周りに開く窓」なので、
> 開いた時点でそのペインへ focus を渡すようにした（ホストのフレームではなく）。
> `FocusedPanelGlobal` は実 focus イベントに従う規約のままで、グローバルの直書きは
> していない。回帰テスト `a_detached_window_focuses_the_pane_it_was_opened_around`。

**該当**: `crates/ravel-app/src/window_host.rs`（`WindowHost::new` の focus）、
`crates/ravel-ui/src/shell.rs`（`handle_reattach`）

detach で開いた窓はホスト自身の `focus_handle` にフォーカスするので、移された
インスタンスは `FocusedPanelGlobal` に入らない。一方で元の窓ではそのパネルの
`on_focus_out` が走って `FocusedPanelGlobal = None` になるため、続けて
`Cmd+Shift+R`（フォーカス窓のパネルをメインへ戻す）を押しても対象が解決できず
何も起きない。**分離窓のパネルを 1 回クリックすれば動く**。

キーボードだけで detach → 即 reattach という自然な操作が沈黙するのが問題で、
ユーザーには「ショートカットが壊れている」ように見える。

→ 分離窓を開いたときに、移送したインスタンスのペインへフォーカスを渡す
（ホストの focus_handle ではなくペイン側）。DOCK-10 の実機確認で決定論的に再現。

---

## MED-APP-23 | gap | 4 つのパネルに View トグルコマンドが無く、メニューから開けない

> **解決済み**: 2026-08-05。`CommandId::ViewToggle{TextEditor,ShaderEditor,
> LuaConsole,RenderQueue}` を追加し、`COMMAND_TABLE` / `label_key()` /
> `for_each_command!` テーブル / View メニュー / ロケール（en / ja）まで配線した。
> 4 つとも既存の `toggle_panel(PanelKind::…)` に乗るので `AppShell::handle_command`
> の分岐は 1 行ずつで、`ViewToggleScopes` のような例外にはしていない。
> **この 4 つにキーバインドは付けていない。** `assets/keybindings/default.toml`
> の `[view]` は `Alt+1`〜`Alt+6` を Timeline / Node Graph / Viewer /
> Properties / Curve Editor / Scopes に割り当てている。当初 `toggle_outliner`
> と `toggle_media_bin` も未割り当てだったが、こちらは常用パネルなので
> **後追いで `Alt+7` / `Alt+8` を付けた**（ユーザー判断、2026-08-05）。
> Text Editor / Shader Editor / Lua Console / Render Queue の 4 つは
> 中身がまだプレースホルダで常用しないため、意図的に未割り当てのまま残す。
> 再発防止の網羅テストは 2 本ある。`ravel-ui` の
> `every_panel_kind_is_reachable_from_a_view_toggle_command` は対応表を
> 書き下さず、`view.toggle_*` の全コマンドを実際に dispatch して各
> `PanelKind` の在否が反転するかで到達性を判定するので、**トグルコマンドの
> 無い `PanelKind` を足すと落ちる**。`every_view_toggle_command_appears_in_the_view_menu`
> はその裏返しで、**View メニューに項目を持たない `view.toggle_*` があると
> 落ちる** — この issue の症状は「コマンドが無い」ではなく
> 「メニューから開けない」なので、メニュー側にも歯止めが要る。
> 開閉そのものは
> `view_toggle_commands_reach_the_editor_and_queue_panels` が固定する。
> これで `REQ-UI-005` の受入条件「全プリセットで全 16 パネルの View トグルが
> 機能する」が埋まった。

**該当**: `crates/ravel-ui/src/command.rs:45-53`（`ViewToggle*` は 9 個）、
`crates/ravel-ui/src/shell.rs:29-35`（`SCOPE_PANELS` が 4 種をまとめて動かす）、
`crates/ravel-ui/src/panel.rs`（`PanelKind::ALL` は 16 種）

`ViewToggle*` は 9 コマンドで、`ViewToggleScopes` が Waveform / Vectorscope /
Histogram / Parade の 4 種をまとめて動かすので、**到達できるのは 12 種**。
残る 4 種 — **TextEditor / ShaderEditor / LuaConsole / RenderQueue** — には
対応するコマンドが存在しない。プリセットが最初から配置していない限り、
ユーザーがそのパネルを開く手段が無い。

ドッキング側の穴ではない。レイアウトモデルは 16 種すべてを扱え、
`every_panel_toggles_into_every_preset`（`ravel-ui`）が 16 × 4 の全組み合わせで
既定スロットへの挿入が成立することを固定している。**欠けているのはコマンド層**。

これが埋まるまで **REQ-UI-005 の受入条件「全プリセットで全 16 パネルの View
トグルが機能する」は満たせない**（`docs/requirements/REQ-UI.md` で未チェックの
まま残してある）。

→ `CommandId::ViewToggle{TextEditor,ShaderEditor,LuaConsole,RenderQueue}` を足し、
`for_each_command!` テーブル・View メニュー・ロケール（en / ja）・
`assets/keybindings/default.toml`（付けるなら）まで通す。
手順は `docs/dev/add-command.md`。既存の `toggle_panel(PanelKind::…)` に乗るので
シェル側の分岐は 1 行ずつ。

---

## MED-APP-24 | bug | View トグルでパネルを開いても実フォーカスが移らず、シェルと GPUI の認識がずれる

> **解決済み**: 2026-08-05。**実 GPUI フォーカスを唯一の真とする側**に寄せた。
> `toggle_panel` は `self.focused` を書くのをやめ、挿入したインスタンスを
> `CommandOutcome::OpenPanel { instance }` としてホストへ返す。
> `RavelWorkspace::dispatch_outcome` がそれを受けて
> `window_host::focus_pane` でそのペインへ**実フォーカス**を渡し、返ってくる
> focus イベントが `track_panel_focus` 経由で `FocusedPanelGlobal` を張り替え、
> その値が次の dispatch の `set_focused_instance` でシェルへ戻る。
> **`focused` を書く経路がホスト由来の 1 本だけ**になったので、2 つの状態が
> 別々に動くことはない（`MED-APP-22` が分離窓側で採った解き方のメイン窓版）。
> detach / reattach の対象決定（`handle_detach` / `handle_reattach`）は無改変で、
> `self.focused` の意味が「シェルの意見」から「実フォーカスの写し」に変わっただけ。
> 回帰テストは `crates/ravel-app/tests/detached_window_host.rs` の
> `view_toggle_focuses_the_panel_it_opened` と
> `detach_after_a_view_toggle_moves_the_opened_panel`。後者は修正前だと
> トグル前に focus していた Viewer のほうが分離されて落ちる。
> `ViewToggleScopes` は 4 枚同時に開くコマンドなので、どれを focus すべきか
> 決まらない。**今回はフォーカスを動かさないまま**にした（シェル側も書かないので
> 乖離は生じない）。
> ダイアログが出ている間は focus を動かさない（`window.has_active_dialog`）。
> パネルは背後で開くが、設定ダイアログに入力中のフォーカスを奪わない。
>
> **起票時の分析はここが間違っていた**（下の本文はその起票時のまま）:
> 「シェルと GPUI が別々に focused を持って**ずれる**」と書いたが、実際には
> `RavelWorkspace::dispatch_command` が `handle_command` の**前**に
> `set_focused_instance(FocusedPanelGlobal)` でシェルを上書きしている
> （`workspace.rs:667-670`）。だから `toggle_panel` の `self.focused = Some(id)` は
> **次のコマンドで必ず捨てられる死んだ書き込み**で、勝負がついた状態が
> 「ずれる」ことは無かった。**常にグローバル側が勝つ。**
> 症状も起票時の記述より単純で、「予測できないほうが分離する」のではなく
> **トグルで開いたパネルは分離対象に一度もならない**（直前に focus していた
> パネルが分離される）。回帰テスト
> `detach_after_a_view_toggle_moves_the_opened_panel` がこれを固定していて、
> 修正前は `left: [Viewer], right: [Dopesheet]` で落ちる。
> 修正の方向（実フォーカスを唯一の真にする）は変わらないが、**動機は
> 「2 つの状態の同期」ではなく「死んだ書き込みを消して、ホストに本物の
> フォーカス移動をさせる」**。

**該当**: `crates/ravel-ui/src/shell.rs:298-311`（`toggle_panel` が
`self.focused = Some(id)` を書く）、`crates/ravel-app/src/panels/mod.rs:931`
（`PanelViews::focus_pane`）、`crates/ravel-app/src/window_host.rs:796`
（その唯一の呼び出し元）

`AppShell::toggle_panel` はパネルを挿入したとき **ヘッドレスの
`self.focused` を新しいインスタンスへ移す**。ところが GPUI ホスト側で
`PanelViews::focus_pane` を呼ぶのは**分離ウィンドウを開くときだけ**で、
メインウィンドウの挿入経路には対応する処理が無い。`focused_panel()` /
`focused_instance()` は `crates/ravel-app/src` から**一度も呼ばれていない**。

結果、View メニューまたは `Alt+N` でパネルを開いた直後に 2 つの状態がずれる:

- **シェル**は新しいパネルを focused とみなす
- **GPUI の実フォーカス**は直前のパネルに残ったまま。タブバーの
  focused 表示は `FocusedPanelGlobal`（実フォーカスイベント由来）なので、
  **画面には古いパネルが focused と出る**

帰結が 2 つ。1 つ目は素直な不便で、開いたパネルにキーボード操作が行かない。
2 つ目のほうが重い: `self.focused` は
`handle_detach`（`shell.rs:353`）と `handle_reattach`（`:383`）の**対象決定に
使われている**ので、パネルを開いた直後に `Cmd+Shift+D` を押すと
**画面上 focused と表示されていないほうのパネルが分離する**。ユーザーには
どのパネルが動くか予測できない。

`MED-APP-22`（分離窓を開いた直後の `Cmd+Shift+R` が沈黙する）と同じ
「シェルの focused と GPUI の実フォーカスの乖離」で、あちらは分離窓側を
`focus_pane` で塞いだ。**これはそのメインウィンドウ側の片割れ**。

`MED-APP-23`（#286）で View トグルが 16 種すべてに届くようになり、
`Alt+7` / `Alt+8` も付いたので、この経路を通る頻度が上がっている。

→ `CommandOutcome` にフォーカス移送を載せるか、ホストがコマンド適用後に
`shell.focused_instance()` と実フォーカスを突き合わせて `focus_pane` を
呼ぶ。**どちらか一方を単一の真とすること** — 2 つの focused 状態を
別々に更新し続けると同じずれが別の経路で再発する。

**検証**: トグルでパネルを開いた直後の `FocusedPanelGlobal` が
そのパネルであることを見る GPUI 統合テスト（実フォーカスに依存するので
ヘッドレスでは足りない）。開いた直後の `Cmd+Shift+D` が
**そのパネルを**分離することのテスト。

## MED-APP-30 | perf | ノードエディタのラバーバンド選択中に Properties が作り直され続ける

**解決済み**: PR #344。因果は起票時の「未確認」どおりで、バンドのマウス移動
ハンドラが毎 move で無条件に選択を公開していた。ガードは全呼び出し側が通る
`set_selected_nodes` の中（`selection_matches` なら早期 return）と、
`CanvasSelection` と `PropertiesTarget` の両方を抑える
`publish_band_selection` に置いた。`refresh_from_document` の意図的な
再公開（選択が同じでも値・露出・driven が動いたとき）は経路が別なので無傷。

**該当**: `crates/ravel-app/src/panels/node_editor.rs:1760-1764`, `:1923-1949`
（バンドの選択公開）、`crates/ravel-app/src/panels/properties.rs:1802`
（`refresh_values_checked`）

ラバーバンドでノードを囲っている間、Properties が目に見えて荒ぶる。

バンドはドラッグ中に選択を公開し、Properties は選択が変わるたびに
セクションを組み直す。`MED-UI-02`（Properties が再生中フレームあたり 2 回
全セクションを再構築）と同じ経路で、**マウス移動のたびに**それが起きる。

`HIGH-28` と同じ再構築経路なので、**ジェスチャ中の再構築を抑える修正が
入れば一緒に収まる可能性がある**。

**未確認**: 「マウス移動ごとに選択が公開されている」ことをソースで特定できて
いない（バンドの公開箇所は `LOW-APP-03` が指す行を参照）。
着手時にまずそこを確かめること。

**修正方針**: バンド中は選択の公開をドラッグ終了までまとめる。
最低でも、前回公開した集合と同じなら公開しない。

## MED-APP-25 | bug | Subnet のコピー＆ペーストが内部グラフの `NodeId` を複製しない

**解決済み**: PR #346。個票が名指ししていたとおり、採番規則を 2 本にせず
既存の再帰に載せた。`Graph::duplicate_nodes_with_fresh_ids` がエッジ無しの
scratch `Graph` を組んで `allocate_duplicate_node_ids` →
`duplicate_with_id_map` → `remap_parameter_node_outputs` にそのまま流すので、
入れ子の全階層の採番も `ChannelSource::NodeOutput` の付け替えも既存コードが担う。

`HIGH-27`（Timeline が Subnet の中へ降りる）の前提でもあった — 行を bare
`NodeId` でアドレスする前提が、内部 ID の重複があると崩れるため。

`NodeEditorPanel::paste_content`（`crates/ravel-app/src/panels/node_editor.rs:1735`）は
ノード自身に `NodeId::next()` を採るが、`node.clone()` が `node.subnet`
（`Arc<Graph>`）を丸ごと写すため、**内部グラフのノード ID が複製元と同一のまま
残る**。

これは `Evaluator` が明文化している不変条件を破る:

```rust
/// (`NodeId::next`), so nodes from every graph (root graph, layer networks)
/// share one registry while cache/dirty state is keyed by full path.
processors: HashMap<NodeId, Arc<dyn NodeProcessor>>,   // crates/ravel-core/src/eval.rs:1348
```

プロセッサ表は平坦な `NodeId → Processor` の写像なので、複製元と複製先の内部
ノードが 1 エントリを奪い合う。

正しい形は既にある。`Graph::duplicate_with_fresh_ids`（レイヤー複製が使う）は
`allocate_duplicate_node_ids`（`crates/ravel-core/src/graph.rs:663`）で
`node.subnet` を**再帰走査して採番し直す**。`paste_content` だけがその再帰を
していない。

**いつから踏めるようになったか**: `NETIF-5`（Add Node から Subnet を作る）と
`NETIF-6`（Collapse to Subnet）が入るまで、Subnet はテストフィクスチャと
デモデータの外に存在できなかった。**ユーザーが Subnet を作れるようになった
時点で初めて到達可能になった**。

**修正方針**: `paste_content` の ID 採番を `allocate_duplicate_node_ids` と
同じ再帰形に寄せる。写像を先に全階層ぶん作ってから
`duplicate_with_id_map` 相当で写すのが既存の形。**2 箇所に別々の採番規則を
置かない**こと。

**検証**: Subnet ノードをコピー＆ペーストして、複製元と複製先の内部ノード ID が
交わらないテスト（`ravel-app` のパネルテストで書ける）。入れ子の Subnet
（Subnet の中の Subnet）でも交わらないこと。

---

## MED-APP-26 | bug | 「プロジェクトへ露出」のトグルが片道 — チェックボックスに見えて解除できない

> **解決済み**: PR #348（2026-08-09）。`toggle_exposed_parameter` が
> `declared` で分岐し、宣言済みなら既にあった `remove_declaration` を呼ぶ。
> どちらの半分が走るかは**描画時のフラグではなくドキュメントを読み直して**
> 決めるので、1 フレーム前の状態で撤回が二重宣言に化けることがない。
> 束縛を辿って名前を取るため、改名済みの宣言も正しく外れる。
> **束縛は一意でない**（`bound_to` の docstring がそう書いている）ので、
> トグルはそのパラメータに束縛された宣言を**全部**外す — 先頭 1 件だけ外すと
> チェックが埋まったままになり、クリックが無視されたように見える。
> ツールチップは状態で出し分ける（`properties.toggle.exposed_remove`）。
> `exposed-parameters-plan.md` の「押し戻しで取り消さない」という判断は
> **この修正で撤回した**（理由は同計画書に記録）。

**該当**: `crates/ravel-app/src/panels/properties.rs:598-631`（`exposed_toggle_button`）、
解除の実体は同ファイル `:2548`（`remove_declaration`）

ボタンは `declared` で `SquareFilled` / `Square` を塗り分け、**チェックボックスとして
描かれている**のに、`on_mouse_down` は状態を見ずに常に `expose_parameter` を呼ぶ。

```rust
let (icon, color) = if declared { (SquareFilled, active) } else { (Square, muted) };
…
.on_mouse_down(MouseButton::Left, move |_, _window, cx| {
    …this.expose_parameter(node_id, &key, cx);   // declared でも同じ
})
```

一度露出させると、その場では戻せない。`remove_declaration` は同じパネルに
既にあり、宣言セクション側の削除ボタン（`:513`）からは呼ばれている
— **繋がっていないだけ**。

対比: ポートの露出は `node_editor.rs:1147` の `toggle_param_port` が
`param_port_index(key).is_some()` で分岐して正しくトグルする。

**修正方針**: `declared` で分岐し、真なら `remove_declaration` を呼ぶ。
`toggle_param_port` と同じ形にする。ツールチップも状態で出し分ける。

---

## MED-APP-27 | bug | Tab で開くノード検索パレットがカーソル位置に来ない（キャンバス中央固定）

> **解決済み**: PR #348（2026-08-09）。`last_pointer`（キャンバスローカル）を
> `on_mouse_move` で持ち、`pointer_or_canvas_center` が使用時にキャンバス矩形の
> 内側かを検査して返す。ポインタがキャンバス外、または一度も乗っていない
> ときだけ従来どおり中央へ落とす。

**該当**: `crates/ravel-app/src/panels/node_editor.rs:2517-2536`（`on_search_palette`）

```rust
let (w, h) = self.canvas_size.get();
let local = (w * 0.5, h * 0.5);      // ← 常にキャンバス中央
```

ダブルクリック経路（`:2816`）は `event.position` を渡しているので、
**同じパレットが開き方によって違う場所に出る**。Tab は手を止めずに使う操作なので、
毎回中央へ視線が飛ぶ。置かれるノードの位置も中央になる。

**修正方針**: 最後のポインタ位置を持っておき、Tab のときそれを渡す。
ポインタがキャンバス外なら現在どおり中央へ落とす。

---

## MED-APP-28 | bug | Timeline のバードラッグが複数選択を無視して 1 レイヤーしか動かさない

> **解決済み**: PR #348（2026-08-09）。**個票の修正方針だけでは足りなかった**:
> バーの mousedown は `LayerClickMode::Replace` で
> `layer_selection_after_click` を通しており、**ドラッグが始まる前に選択が
> 1 枚へ潰れていた**。押下時点で「掴んだバーが既に選択に含まれるなら選択を
> 保つ」に変え、動かさずに離したときだけ mouseup で 1 枚へ絞る
> （`MoveKeyframe` が既に使っていた `collapse_on_click` と同じ規則）。
> そのうえで `MoveBar` / `TrimIn` / `TrimOut` の 3 つが
> `Vec<BarBaseline>` を持ち、`operation_targets` からロック済みを除いて作る。
> **トリムも含めた**（個票が「同じ形」と書いていたもの）。制限は
> レイヤーごとに自分の表示区間で掛かる。1 マウス移動 = 1 `apply_document`。
> ジェスチャが**始まらない**押下（修飾クリック / ロック済み / バーを外した
> 押下）は普通のクリックなので、その場で選択を 1 枚へ絞る — mouseup 側の
> 絞り込みは走らないため。判断は `press_layer_bar` 1 箇所に置いた。

**該当**: `crates/ravel-app/src/panels/timeline.rs:1491-1512`（`drag_moved` の
`TimelineDrag::MoveBar`）

```rust
TimelineDrag::MoveBar { layer, origin_start, grab_x, .. } => {
    …
    self.edit_layer(layer, …, |l| l.start_frame = new_start, …)
}
```

`MoveBar` は**単一の `LayerId`** を持ち、ドラッグはその 1 つだけを動かす。
複数レイヤーを選択してバーを掴んでも、掴んだ 1 本しか動かない。

削除（`delete_layer`）と複製（`duplicate_layers_from_row`）は
`operation_targets` を通して選択全体へ広げているので、**バードラッグだけが
選択の規約から外れている**。トリム（in / out）も同じ形。

**修正方針**: `MoveBar` に対象集合を持たせ、`operation_targets` で決める。
1 ジェスチャ = 1 undo は維持する（各レイヤーに同じ差分を当てて 1 コミット）。
ロックされたレイヤーは `delete_layer` と同じく保護する。

---

## MED-APP-31 | bug | ポップアップメニューが開いている間もワークスペースのショートカットが勝つ

> **解決済み**: PR #349（2026-08-09）。**個票の診断は半分だけ当たっていた。**
>
> - **矢印は個票どおり。** gpui は同じ深さのバインドを登録順で決め
>   （`Keymap::bindings_for_input` は深さ降順 → index 降順）、Ravel は
>   `gpui_component::init` の後に束縛する。述語を
>   `!Input && !PopupMenu && !AppMenuBar` に広げて降りるようにした。
>   組み立ては `workspace::workspace_binding_context` の 1 箇所で、
>   文脈名は `gpui_component::menu` の `POPUP_MENU_CONTEXT` /
>   `APP_MENU_BAR_CONTEXT`（この PR でフォークに `pub` として生やした）を
>   参照するので、Ravel は文字列を二重に持たない。
>   **パネル固有のバインドも同じ narrowing を受ける**（`yield_to_open_menus`。
>   個票に無い判断）— ポップアップは開いたパネルの子として dispatch tree に
>   載るので、パネルの文脈はメニューが開いている間もスタックに残り、
>   narrowing が無いと `L`（自動整列）がメニューの裏で走る
> - **Escape は別原因だった。** `DropdownMenuPopover` は `PopupMenu` を
>   **初回生成時にしか focus していなかった**（`dropdown_menu.rs` の `None`
>   分岐）。キャッシュを捨てるのはメニュー自身の `DismissEvent` のときだけで、
>   トリガー再クリックや外側クリックで閉じた場合は残る。一方
>   `Popover::toggle_open` は開くたび**自分の** focus handle を取る
>   （`DropdownMenu` は `track_focus` を呼ばない）。結果、**2 回目以降に開いた
>   ドロップダウンは PopupMenu が focus を持たず**、矢印も Enter も Escape も
>   死ぬ。フォーク側で `on_open_change` を足し、閉じたらキャッシュを捨てて
>   次の開閉で必ず組み直して focus するようにした
>
> **述語の副作用を意図として記録する**: 否定文脈はその文脈がスタックにある間
> バインドを丸ごと無効にするので、メニューが開いている間は Space も含め
> どのワークスペース chord も発火しない。開いているメニューはキーボードに
> 対してモーダル、という判断。

**該当**: `crates/ravel-app/src/workspace.rs:424`（`build_keybindings` が
アセット由来のバインドに与える文脈 `"!Input"`）、
`crates/ravel-app/src/main.rs:64` / `:100`（登録順）、
`assets/keybindings/default.toml`（`playback.step_forward` = `Right`、
`step_backward` = `Left`）

`gpui_component` のポップアップは自前のキー操作を持っている。
`PopupMenu` は上下と Enter、`AppMenuBar` は左右でのトップレベル移動を
それぞれ専用の文脈（`PopupMenu` / `AppMenuBar`）に登録する
（`gpui_component::init` 内）。

**それが Ravel 側のバインドに潰される。** アセット由来のコマンドは
すべて `"!Input"` 文脈で登録され、これは**テキスト入力しか避けていない**。
ポップアップの文脈は除外していないうえ、Ravel の `cx.bind_keys` は
`gpui_component::init` より**後**に走るので、同じ和音では Ravel 側が勝つ。

結果、**メニューを開いた状態で ← → を押すとトップレベルが動かずフレームが
送られ、メニューが閉じる**。

在窓のアプリメニューバー（非 macOS、`HIGH-29` で入った）で目に見えるが、
**バー固有ではない**。パネルの `…` ドロップダウンなど、**すべての
`PopupMenu` に同じことが起きる**。到達不能にはならず（マウスで操作できる）、
macOS では OS のメニューバーなので影響しない。

**未確認**: **Escape でポップアップが閉じない**現象も同時に観測されている
（既存のパネル `…` ドロップダウンでも同じ）。ただし Ravel は Escape を
アセットにもコードにも束縛しておらず、`DockRoot` の `observe_keystrokes`
（`crates/ravel-dock/src/dock.rs:223`）はキーを消費しない観測なので、
**上記の登録順とは別の原因**。着手時に切り分けること。

**修正方針**: アセット由来のバインドの文脈述語を、テキスト入力だけでなく
**ポップアップの文脈も避ける**形にする。文脈名は `gpui_component` 側の
定数が正で、Ravel が文字列を二重に持たないこと。
全ワークスペースコマンドの経路に触るので、`for_each_command!` の 1 表を
通る変更として入れる。
