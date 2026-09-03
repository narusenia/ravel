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
>
> **実機確認済み**（2026-08-09）。手順は「パネルの `…` を開く → 外側クリックで
> 閉じる → もう一度開く → Escape」で、修正前は閉じなかったものが閉じる。
>
> **自動テストは無い。** 述語のテストが見ているのは `eval()` の意味論で、
> 実行中のアプリで `PopupMenu` が dispatch stack に載っているかは見ていない。
> フォーク側の再 focus も同様（ヘッドレスで `PopupMenu` を開くと gpui の
> テストウィンドウが panic する）。同じ症状が再発したら、まず上の手順で
> 再現するかを見ること。

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

---

## MED-APP-32 | bug | EXR / HDR のサムネイルが暗い（リニア値を表示変換なしで量子化する）

**該当**: `crates/ravel-app/src/media/thumbnail.rs:397`（`encode_thumbnail`）、
`:455`（`read_image_frame`）

> **解決済み**: サムネイル生成が解決済みの入力色空間
> （`MediaAssetEntry::input_color_space()`）を受け取るようになり、
> `read_image_frame_in` / `with_input_color_space` で作業空間へ読んでから
> `to_display_rgba8`（解決済み入力 → 作業空間 → `ColorSpace::DISPLAY`）で
> 量子化する。sRGB 素材ではこの往復が恒等なのでバイト列は従来と同一
> （`srgb_frames_keep_their_bytes`）、リニア素材は display 空間で生成される
> （`linear_frames_are_display_encoded`）。色空間はディスクキャッシュの
> derivative key にも入った。`lint-patterns.allow` の
> `raw-pixel-quantisation` 免除は不要になったので削除。

サムネイルは**ファイルそのものの値**を読む。`ThumbnailSource::Still` /
`Sequence` は `image_seq::read_image_frame` を、`Container` は
`FfmpegDecoder::open` を通り、どちらも入力色空間の既定
（`ColorSpace::WORKING` = 無変換）なので伝達関数は外れない。
そのうえで `encode_thumbnail` が

```rust
(component.clamp(0.0, 1.0) * 255.0).round() as u8
```

と**直接量子化**する。

**整数素材ではこれが正しい。** ファイルが sRGB で符号化された値を持ち、
それをそのまま量子化するので、元のファイルと同じ絵が出る。表示変換を
足すと逆にすべてのサムネイルが明るくなる。`scripts/lint-patterns.allow` の
`raw-pixel-quantisation` 免除はこの理屈で入っている。

**float 素材では成立しない。** EXR / HDR が持つのは**リニア値**なので、
それを sRGB 符号化済みとみなして量子化すると暗くなる。リニア 0.5 は
sRGB では 188 だが、この経路は 128 を書く。中間調ほど落ち込みが大きく、
**素材ビンの EXR だけが一様に暗いサムネイルになる**。

**影響**: 表示だけの問題で、合成にも書き出しにも波及しない。ただし
素材ビンは「どのファイルか」を絵で選ぶ場所なので、露出の判断ができない。

**修正方針**: **解決済みの入力色空間が作業空間（リニア）だったときに**
表示変換を通す（`ravel_core::color::to_display_rgba8`）。

**「float なら」ではない。** 判定の軸はビット深度ではなく色空間で、
明示指定やメタデータで `LinearRec709` と解決された整数素材も変換が要る。
逆に `Rec709` / `Rec2020 + PQ` と解決された素材は**そのまま量子化しては
いけない**（display-referred なのは sRGB の場合だけ）。厳密には
「解決済み入力色空間 → `ColorSpace::DISPLAY`」の変換を通すのが正で、
sRGB 素材ではそれが恒等になるので現状の見え方が保たれる。

そのためには**この呼び出し側が持っていない情報**が要る — 素材の入力色空間。
`MediaAssetEntry::input_color_space` はプロジェクト側の解決結果で、
サムネイル生成はそれを受け取っていない。素直な形は
`read_image_frame_in` に解決済みの色空間を渡し、作業空間で読んだうえで
表示変換を掛けること。**中で拡張子を見て分岐するのはやってはいけない** —
解決順（明示指定 > メタデータ > 拡張子既定）が 2 箇所に分かれる。

**関連**:
- [HIGH-31](HIGH-31-float-decode-through-8bit-rgba.md) — 同じ EXR が
  8bit を経由して取り込まれるので、暗さに加えて 1 超がクリップされている。
  暗さだけ直しても白飛びは残る
- `MED-MED-07`（[media-audio.md](medium-media-audio.md)）— 入力色空間の解決に
  メタデータが効いていない。この修正が渡す値の質はそちらに依存する

---

## MED-APP-33 | bug | `ravel-cli` が設定層を一切読まないので、ヘッドレスレンダーが `settings.toml` と `.ravprj` の設定を無視する

**該当**: `crates/ravel-cli/src/lib.rs:219`

`ravel-cli` は `ResolvedSettings` を `::default()` としてしか使わない。
`ResolvedSettings` への参照はこの 1 箇所だけで、`read_global_settings` に
相当する呼び出しがどこにも無い。コメントもそう明言している:

```rust
// Settings layers are not loaded by the CLI, so the limits are the
// canonical defaults — the same ones `ProjectState` starts from.
let budget = SharedCacheBudget::new(ResolvedSettings::default().cache_budget());
```

`SET-8`（#374）が GUI 側でキャッシュ設定を走行中の予算へ配線したことで、
**同じ `settings.toml` を GUI とヘッドレスで読ませたときに挙動が分かれる**
ようになった。ユーザーが VRAM / RAM の上限や sim 予約率を絞っても、
`ravel-cli render` はそれを見ない。プロジェクト層（`.ravprj` の `[cache]`）も
同様に届かない。

`SET-8` が入れた検証（範囲外の値と相対パスを弾く）もこの経路には掛からない。

## 影響

レンダーファームやバッチ用途で**メモリ上限を指定する手段が無い**。
GUI で調整した設定がそのまま効くという期待も裏切る。
`SET-8` 以前は GUI 側も既定だったので差が無く、この単位が差を作った。

## 修正方針

1. `ravel-cli` の起動時に設定層を解決する（グローバル層 + 開いた
   `.ravprj` のプロジェクト層）。`ravel-project` は GUI-free なので
   依存の追加は要らない
2. 解決した設定を予算へ流す。**範囲検証を二重に書かないこと** —
   `SET-8` は `app_settings` に `cache_limit_mb` / `cache_sim_reserve_ratio` /
   `cache_root_setting` を置いたが、それは `ravel-app` にあるので
   `ravel-cli` からは呼べない。共有するなら `ravel-project` へ下ろす判断が要る
3. CLI から層を上書きする口（`--cache-vram-limit` 等）を出すかは別の判断。
   **まず設定ファイルが効くようにするのが先**

## 検証

- `settings.toml` に上限を書いてヘッドレスレンダーを回し、予算がその値になること
- 範囲外の値と相対パスが GUI と**同じ規則で**弾かれること

## 関連

- `docs/implementation/settings-screen-plan.md` の `SET-8`（#374）— 差を作った単位
- `MED-APP-10`（同ファイル）— 設定が効かない件の本体（GUI 側は解消済み）

**解決済み**: この PR。global → project の設定層を `ravel-cli` の予算へ接続し、
検証関数を `ravel-project::settings` に集約した。

---

---

> **解決済み**: PR #419（2026-08-13）。カーブ点のドラッグと行の高さドラッグが
> `pressed_button` を検査し、ボタンを失ったらドラッグを**コミットして**終える
> （見えている編集を捨てない）。`param_ramp_editor.rs` が `PARAM-4` で入れた形に
> 揃えた。回帰テストは `a_point_drag_ends_and_commits_when_the_button_is_lost` と
> `losing_the_button_ends_an_inline_editor_resize`。

## MED-APP-34 | bug | パラメータエディタのドラッグが `pressed_button` を確認せず、行の高さドラッグも後始末が無い

**該当**: `crates/ravel-app/src/widgets/param_curve_editor.rs:1571-1581`（カーブ点の
`on_drag_move`）、`crates/ravel-app/src/panels/properties.rs:908-920`（行の高さドラッグ）

`MED-APP-03`（NodeEditor）と同じ形の穴が Properties 側にも 2 箇所ある。

1. **カーブ点のドラッグ**が `event.pressed_button` を確認しない。ウィンドウ外で
   左ボタンを離してから戻ると、ボタンを押していない状態でドラッグが継続する。
   `on_mouse_up_out` が付いているので窓外の離しは拾えるが、**離した後の移動
   イベント**は素通しになる
2. **行の高さドラッグ**（アコーディオンのリサイズ）に `pressed_button` の検査も
   mouse-up-out 相当の後始末も無い。リサイズ中にボタンを失うと `row_resize` が
   残留する

`viewer.rs:1741-1747` と `timeline.rs:3464-3467` が同じ問題に対する防御を持って
いるので、**形は既にリポジトリ内にある**。

**ランプエディタ側は防御済み**（`param_ramp_editor.rs` の `on_drag_move` が
`pressed_button != Some(Left)` で `end_drag_without_pointer` を呼ぶ）。
カーブ側が同じ形に揃っていないだけ。

**修正方針**: `event.pressed_button != Some(MouseButton::Left)` のとき、
カーブは `end_drag`（ランプと同じく**コミットする** — 見えている編集を捨てない）、
行の高さは `row_resize` をクリアする。

**検証**: ボタン喪失後の移動イベントでドラッグが継続しないテスト
（`param_ramp_editor` の同種テストに倣う）。


---

---

> **解決済み**: PR #419（2026-08-13）。`gesture_row_disappeared` が「ジェスチャの
> 対象行が消えた」を検出し、次の render で `end_gestures` を通してから再構築する。
> **行が同じままの再構築は従来どおり延期**する。
>
> 3 種のうちガードが効くのは**スクラブ行だけ**で、それが正しい —
> `ParameterValue::Curve` / `Ramp` は構造パラメータで `port_accepted_types` が
> 空を返すため driven 化できず、カーブ行・ランプ行が消える経路はノード削除だけ
> （既存経路が処理する）。回帰テストは
> `a_scrub_ends_before_rebuild_when_its_row_becomes_driven` ほか 3 本。

## MED-APP-35 | bug | ジェスチャ中に行が消えると Properties が来ない release を待ち続ける

**該当**: `crates/ravel-app/src/panels/properties.rs:2121-2129`, `:2148-2162`, `:4235-4239`

`gesture_in_flight()` が真の間、外部由来の再構築は `needs_rebuild` を立てるだけで
延期される（ジェスチャと再構築が喧嘩しないための設計）。ところが**ジェスチャの
対象そのものが消える**経路がある:

- ドラッグ中に別経路（他パネル、undo、ノード接続）でそのパラメータが
  driven になる、または行ごと消える
- 行がツリーから外れるので、**mouse-up / release イベントがもう届かない**
- ウィジェットの `drag` は立ったまま → `gesture_in_flight()` が真のまま →
  再構築が永久に延期される

カーブ行・ランプ行・スクラブ行のすべてに同じ形で存在する（**ランプ固有では
ない**。`PARAM-4` の実装で発見されたが、`PARAM-2` の時点で同じ構造）。

**修正方針**: 再構築を延期するのではなく、**行が消えるときにそのジェスチャを
強制終了して undo ステップを取る**（`end_gestures` を再構築の前に必ず通す）。
`caf929a`「ジェスチャは始めた対象の上で確定する」と同じ規律を、対象が
消える場合へ広げる形になる。

**検証**: ドラッグ中に対象パラメータを driven 化し、(1) undo ステップが 1 つ
記録される、(2) 再構築が延期されずに走る、を落とすテスト。

---

## MED-APP-21 | debt | Viewer の bbox が `type_key` の固定 match でパラメータから再構成される

**該当**: `crates/ravel-app/src/panels/viewer.rs:2388-2423`, `:453`, `:527`

`shape_node_bounds` はジオメトリを評価せず、`type_key` の match で
パラメータ名を直読みして矩形を作る。

```rust
"shape.rect"    => (width * 0.5, height * 0.5)
"shape.ellipse" => (radius_x, radius_y)
"shape.polygon" => (radius, radius)
"shape.star"    => (outer_radius, outer_radius)
```

帰結が 3 つ:

1. shape ノードを追加するたびにこの match を編集しないと bbox が出ない
   （`geometry-ops-plan.md` 単位 11 の `shape.line` / `shape.grid` が該当）
2. `geometry.transform` や `scatter.*` を経た**実際の形状が反映されない**
3. `docs/specifications/procedural-geometry.md` の設計原則 1
   「固定機能のリピーターを作らない」に対する既存の例外

ドラッグ経路（`:453`, `:527`）も同じ関数に依存している。

**修正方針**: 評価済み Geometry から bbox を出す。設計と実装単位は
`docs/implementation/done/viewer-overlay-manipulator-plan.md` 単位 3
（`shape_node_bounds` の廃止を含む）。**推測値と実測値を並存させない** —
並存させると評価前後で bbox が飛ぶ。

**検証**: `type_key` を知らないノードで bbox が描かれるテスト。
`geometry.transform` を経た形状の bbox が変換後になるテスト。

> **解決済み**: `done/viewer-overlay-manipulator-plan.md` 単位 3。`shape_node_bounds` を
> 削除し、bbox・点・パス・クリック判定・レイヤードラッグのすべてを**評価済み
> Geometry** から引くようにした（`crates/ravel-app/src/panels/viewer/geometry.rs`）。
> 対象ノードの評価は `EvalRequest::scoped` に載せてコンプ要求と同じ
> `Evaluator` から引くので、シェル評価が既に走らせたノードはキャッシュヒットになる。
>
> **推測値と実測値は並存しない**: 結果が未着なら描かず、クリックも当たらない。
> `type_key` を知らないノードで bbox が出ること、`geometry.transform` 後の
> bbox が変換後になること、`scatter.*` の全インスタンスが点として描かれることを
> それぞれテストで固定した。

## MED-APP-36 | bug | 殻マニピュレータが「レイヤー 1 枚だけ選択」で出ない（評価要求の相互待ち）

> **解決済み**: 2026-08-24。`ShellManipulator::is_active` を**選択だけ**から
> 決める形へ直し、`eval_targets` を実装して自分が必要とするジオメトリ評価を
> 自分で頼むようにした（`GeometryOverlay` と同じ規律）。描画とハンドルは従来
> どおり `ShellState::resolve` を通すので、評価が届くまでは何も描かず何も
> 掴めない。回帰テストは
> `one_selected_layer_requests_its_own_geometry_evaluation`
> （`crates/ravel-app/src/panels/viewer/overlay.rs`。旧実装では
> 「nobody asked for the selected layer's geometry: []」で落ちる）。

**該当**: `crates/ravel-app/src/panels/viewer/overlay.rs:1248-1260`（`GeometryOverlay::networks`、
`BboxScope::Layer`）、`:2027`（`ShellManipulator::is_active`）、`:852`（`eval_targets` の既定実装）

`ShellManipulator::is_active` は `ShellState::resolve(ctx).is_some()` で、これは
`layer_comp_rect` → `geometry::evaluated_bounds` → `ctx.eval_result(..)` と辿るので
**そのレイヤーのジオメトリノードの評価結果が既に手元にあること**を要求する。
ところが `ShellManipulator` は `eval_targets` を実装していない（既定は空 Vec）ので、
その評価を頼むのは `GeometryOverlay` だけ。そして `BboxScope::Layer` の
`networks()` は **`layer_selection.layers().len() < 2` で空を返す** ため、
**レイヤーを 1 枚だけ選んでいるときは誰も頼まない** → 結果が来ない →
`is_active` が false のまま。`GeometryOverlay::is_active` の doc コメントが
「結果から is_active を決めると、頼まれないから結果が来ない相互待ちになる」と
警告している、まさにその形。

`BboxScope::Node`（ノード選択）が同じネットワークのジオメトリを頼むので、
**Node Graph でノードを選んでいる間は出る**。だから普段の作業では気づきにくい。

**再現**（実機、2026-08-24 / macOS）:
1. 新規プロジェクト → `Layer ▸ Add Shape Layer`
2. Outliner でそのレイヤーをクリック（`set_layer_selection` は走る。
   Properties も Transform を出す）
3. Viewer は Select ツール。**bbox もハンドルも出ず、図形をドラッグしても
   `position` は動かない**（Properties の数値編集でしか動かせない）

`docs/ui-impl-status.md` の「レイヤー殻のマニピュレータ」は ✅ と書いており、
**実装状況の記述と挙動が食い違っている**。

→ `ShellManipulator` に `eval_targets` を実装し、選択中の 1 枚について
`geometry::geometry_targets(document, &NetworkPath::layer(comp, layer))` を返す
（`GeometryOverlay` と同じ形）。`is_active` を結果に依存させたままにするなら、
**その結果を自分で頼むのが唯一の直し方**。

**検証**: レイヤー 1 枚選択・ノード選択なしの状態で `eval_targets` が空でない
テスト。オーバーレイの登録経路（`OverlayRegistry::builtin`）越しに、1 枚選択で
`ShellManipulator` の要求が畳み込まれることを検査する。

---

---

## MED-APP-15 | debt | Hand / Zoom ツールが機能しない dead UI

**該当**: `crates/ravel-app/src/panels/viewer.rs:1242-1249`, `:1795-1815`

> **解決済み**: `TOOLX-1`。Hand の左ドラッグは中ボタンと同じ `pan_mouse_down` /
> `pan_dragged` / `pan_ended` へ、Zoom のクリックは `zoom_toward`、ドラッグは
> 新設の `ViewerViewport::zoom_to_rect` へ流れる（`Alt`+クリックで縮小、倍率は
> スクロールズームと同じ `zoom_factor` の段）。左ボタンがどのジェスチャーに
> なるかは `ViewerPanel::left_mouse_down` の網羅 `match` 1 か所が決めるので、
> Hand / Zoom 中は選択・シェイプ描画・ペン・オーバーレイハンドル・ガイドの
> どれも始まらない。カーソルも同時に付いた（Hand = `OpenHand`、パン中 =
> `ClosedHand`、Zoom = `Crosshair`）ので、`done/pointer-feedback-plan.md` の
> 保留も解消している。

ツールバーは Hand / Zoom を提供し 'H' 押下で Hand に切り替わるが、
Hand の左ドラッグパンも Zoom のクリックズームもハンドラが存在しない
（中ボタンドラッグのみがパンし、それはどのツールでも動く）。
選択すると左ボタン編集が無効化されるだけ。

**修正方針**: Hand は左ドラッグをパン経路へ、Zoom はクリック / alt+クリックを `zoom_toward` へ
ルーティングする。または実装まではツールを外す。

**引受先**: `docs/implementation/done/viewer-tool-extensions-plan.md` の `TOOLX-1`
（実装する方を採る）。`docs/implementation/done/pointer-feedback-plan.md` は
この 2 ツールのカーソルを意図的に見送っており、`TOOLX-1` がカーソルも同時に付ける
（機能が無いものに UI の約束をしないため）。
