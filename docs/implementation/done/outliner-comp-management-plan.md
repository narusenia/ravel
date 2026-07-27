# Outliner + コンポジション管理実装計画（REQ-UI-013）

> **Status**: Done — 単位 1〜6 完了（2026-07-25 設計確定）

## 問題

`Document` は `compositions: im::HashMap<CompId, Arc<Composition>>` と
`root_comp: Option<CompId>` を持ち、モデルとしては複数コンポジションに
対応している。しかし UI 側は `root_composition(doc)` を直参照する経路が
`project_state` / `panels::timeline` / `panels::viewer` に散在しており、
コンポジションを作る・切り替える・設定を変える手段が存在しない。
起動時の root comp は `default_document()` の固定値（`Comp 1` /
1920×1080 / 30fps / 300f）で、解像度ひとつ変えられない。

Outliner は `PlaceholderPanel` のまま（`docs/ui-impl-status.md` で 🔲）。
レイヤー選択は `TimelineState.selected_layer` というパネル内部状態で、
そこから Node Editor / Properties / `CanvasSelection` が駆動されている
（#151）。Outliner でもレイヤーを選べるようにすると二重管理になる。

要件と設計判断は `docs/requirements/REQ-UI.md` の REQ-UI-013 に確定済み
（2026-07-25 設計セッション。3階層ツリー採用、`net.out` 上流トラバース、
active と root_comp の分離、コンプ 0 の許容、ダイアログ + Properties の
二本立て、非アクティブコンプの子行はダブルクリックのみ、といった経緯込み）。

前提となる既存基盤: `CanvasSelection` / `ToolState` の durable Global 方式
（#138 / #139）、`NetworkPath` と Node Editor の subnet dive、
`PropertiesTarget`（`Empty` / `Nodes` / `Layer`）と「ターゲットは対象を
identify するだけで値は毎回ドキュメントから解決する」規約、
`Graph::inputs_of()`、`for_each_command!` コマンドテーブル、
`.ravprj` の zip コンテナ（`manifest.json` / `document/main.ron` /
`assets/refs.json` / `settings.toml` + format_version 3 の migration チェーン）、
gpui-component の `Dialog`（`window.open_dialog`、overlay / Esc / focus_trap /
on_ok / on_cancel）と既に導入済みの `Root`。

## 目標アーキテクチャ

- **アクティブコンプの正**: `ActiveComposition(Option<CompId>)` durable
  Global（ravel-app panels、`CanvasSelection` と同じ場所・同じ方式）。
  Timeline / Viewer 評価 / PlaybackController の duration・fps /
  Properties / Outliner はすべてこれを読む。`root_composition()` 直参照は
  全廃し、`ActiveComposition` 解決経路に置換する。
  `Document.root_comp` は「ドキュメントを開いたとき最初に active になる
  コンプ」としてモデルに残し、UI の切替では書き換えない
  （コンプ切替を undo 履歴と保存差分に載せないための分離。将来 PreComp が
  入ったとき「出力ルート」と「編集中」が別概念になる前提でもある）。
- **選択の正**: `LayerSelection { comp: Option<CompId>, layers: Vec<LayerId> }`
  durable Global。`layers` は順序保持（範囲選択の起点判定と表示順の安定）。
  `TimelineState.selected_layer` は廃止し、Timeline と Outliner の双方が
  この Global を読み書きする。Node Editor / Properties / Viewer bbox は
  observe するだけ。ノード選択は既存 `CanvasSelection` のまま。
  **不変条件**: `LayerSelection.comp == ActiveComposition`。
  この不変条件を保つために、非アクティブコンプの子行の選択は
  「active 切替 + 選択」の複合操作としてのみ発生させる。
- **Outliner の構造**: ヘッドレスなツリー構築とツリー状態（展開集合）は
  `crates/ravel-ui/src/panels/outliner.rs` に置き、GPUI 描画と入力は
  `crates/ravel-app/src/panels/outliner.rs`（Timeline と同じ分割）。
  ツリー行は `OutlinerRow { depth, kind }`、`kind` は
  `Comp(CompId)` / `Layer(CompId, LayerId)` / `Node(NodeId, NodeRole)` /
  `UnusedGroup` の平坦なリストとして生成し、描画側は行を上から描くだけに
  する（`Render` 内でグラフを辿らない）。
- **ノード行のツリー化**: `net.out` を根に `inputs_of()` を深さ優先で辿る
  （`net.out` 自身は行にしない）。`HashSet<NodeId>` の visited で既出
  ノードは子を展開しない参照マーク付きの葉にする（DAG の指数膨張防止）。
  `net.out` から到達しないノードは末尾の Unused グループ。サブネット
  ノードは葉 + バッジで、ダブルクリックは Node Editor の既存 dive
  （`NetworkPath::entered`）に委譲する（Outliner のツリー深さは 3 階層固定）。
- **コンプ設定の編集経路**: `PropertiesTarget::Composition { comp_id }` を
  追加し、Properties が名前 / 解像度 / fps / duration / 背景色を常時編集
  できるようにする（値は `ParameterValue` ではない素フィールドなので
  キーフレーム不可の専用セクションを手書きする）。加えて新規作成と明示的な
  設定編集は `Dialog` で行う。フィールド描画は共通関数に切り出して
  Properties とダイアログの両方から呼ぶ。新規作成をダイアログにするのは、
  未確定のコンプをドキュメントに作ってから直す形（undo 2 段）を避けるため。
- **永続化**: `.ravprj` に `ui_state.json` エントリを新設し
  `{ "active_comp": <u64> }` を保存する。エントリ欠落時は `root_comp` に
  フォールバックするので **format_version の引き上げは不要**（既存 v3
  アーカイブと互換）。将来の UI 永続状態（Outliner の展開集合、Node Editor
  のビュー位置など）もこのエントリに集約する器とする。
  `Document` には入れない — `DocumentStore` の undo は Document 全体
  スナップショットなので、コンプ切替が undo/redo で巻き戻ってしまう。
- **コマンド経路**: `CompositionNew` / `CompositionSettings` /
  `CompositionDuplicate` / `CompositionDelete` を `CommandId` +
  `for_each_command!` に追加し、メニューバーに Composition メニューを新設。
  Outliner のボタンと右クリックも同一 Action を dispatch する
  （`.agents/rules/gpui.md` のコマンド経路単一性）。`CompositionSettings`
  にキーバインド Cmd+K を割り当てる（既存割当と衝突しないことを実装時に確認。
  衝突する場合はメニューのみとし、要件側の記述も合わせて直す）。
- **コンプ 0 の状態**: 正当な状態として扱う。`ActiveComposition(None)` /
  `LayerSelection.comp == None` で Timeline / Viewer / Properties /
  Outliner が空状態を描き、Outliner から新規作成できる。

## 実装単位

1. **状態の一元化（core/ui/app、挙動不変のリファクタ）**:
   `ActiveComposition` と `LayerSelection` Global を新設し、
   `root_composition()` 直参照（`project_state` / `timeline` / `viewer`）と
   `TimelineState.selected_layer` を全面置換する。Playback の duration/fps
   解決も active 経由にする。コンプ 0 = `None` 経路の空状態処理を各消費者に
   入れる。この単位では active は常に `root_comp` なので**見た目の挙動は
   変わらない**（大きな配線変更を単独でレビュー・実機確認できる）。
2. **アクティブコンプの永続化（app）**: `ui_state.json` エントリの
   読み書き（`container::entry` 追加、save/load 経路、欠落時フォールバック）。
   既存 v3 アーカイブの読み込み互換テストと round-trip テスト。
3. **Outliner パネル本体（ui/app）**: ヘッドレスなツリー構築
   （3階層・`net.out` 上流トラバース・visited による参照葉・Unused
   グループ・サブネット葉）と展開状態、GPUI 描画、クリック意味論
   （コンプ: シングル選択 / ダブル active 切替、レイヤー・ノード:
   シングル選択 / ダブルで Node Editor センタリング、非アクティブコンプの
   子行はシングル無反応・ダブルで active 切替 + 選択）、`LayerSelection` /
   `CanvasSelection` への書き込みと observe、空状態。ロケール（en/ja）。
   Node Editor 側にビューセンタリング API が無ければここで追加する。
4. **コンプ管理コマンドと設定 UI（ui/app）**: `CommandId` 4 つ +
   Composition メニュー + ロケール、新規作成ダイアログ（初期値は
   active → `manifest.json` の project 既定 → 1920×1080/30fps/300f）、
   設定ダイアログ、`PropertiesTarget::Composition`、複写（fresh id・
   一意名・複写後 active 切替）、削除（レイヤーを含むときのみ確認、
   削除後の active は同じソート順で隣、undo 1 回）。
5. **Outliner のレイヤー操作（app）**: D&D による同一コンプ内の並べ替え
   （既存 `reorder_layer`）、右クリックメニューの Rename（インライン編集）/
   Duplicate / Delete。親子付け替え D&D と solo/mute/lock 列は入れない。
6. **複数選択の完成**: 前半 = Shift 範囲 / Cmd トグルの選択 UI（Timeline の
   ヘッダー行・バーと Outliner のレイヤー行）、Node Editor の中央メッセージ、
   Properties の複数表示（読み取りのみ）。後半 = 複数同時ドラッグ移動・
   レイヤー単位の bbox・一括削除・一括 solo/mute/lock・一括複写。
   複数同時ドラッグは REQ-UI-011 の `center_x/y` 再構築方式を複数レイヤー分
   まとめて 1 undo にする必要があるため、単位 4/5 と混ぜない。
   **スコープ判断（前半で確定）**: Viewer の bbox は `CanvasSelection`
   （1 レイヤーのネットワーク内ノード）駆動で、レイヤー単位の bounds を
   持っていない。前半は「複数レイヤー選択時に古い bbox を描かない」ことだけを
   保証し（Node Editor が閉じるときに `CanvasSelection` をクリアするので
   `selection_comp_rects` は空になる）、レイヤー単位 bbox の描画は同時ドラッグと
   同じ後半に入れる — ドラッグ対象の可視化なので、描画と移動を別 PR に
   割るとどちらも中途半端になる。

依存: 1 → 2 / 3 / 4（3 と 4 は並行可能）、3 → 5、1 と 3 → 6。PR は単位ごと。

## 完了条件（単位別）

- 単位 1 ✅: `root_composition()` の直参照が UI 経路に残っていない
  （grep で確認。残るのはヘッドレスなドキュメントを検査するテストのみ）。
  既存のタイムライン・Viewer・Properties の挙動と既存テストが不変。
  `ActiveComposition(None)` / `LayerSelection.comp == None` で各パネルが
  パニックせず空状態を描く。
  実装メモ: Properties ターゲットは選択から導出される状態として
  `set_layer_selection` / `set_active_composition` が寿命を持つ
  （`Nodes` ターゲットは横取りしない）。Node Editor は単位 1 では
  引き続き Timeline からの push で追従する — `LayerSelection` の observe
  への切り替えは Outliner が第二の書き手になる単位 3 で行う。
  Playback の fps/duration は Timeline パネル経由をやめて
  `ProjectState::playback_params` から解決する。
- 単位 2 ✅: 保存 → 再読込でアクティブコンプが復元され、`ui_state.json` を
  持たない既存アーカイブが `root_comp` フォールバックで読める。
  実装メモ: `UiState` は `#[serde(default)]` で未知フィールドを無視する
  （新しい Ravel が書いた器を古い Ravel が読める）。壊れたエントリは
  ロードを失敗させず警告 + デフォルトに縮退する（UI 状態のために
  プロジェクトを開けなくしない）。フォールバックは
  `UiState::initial_active_comp(document)` の 1 箇所に閉じ、消えたコンプを
  指すエントリも root へ落とす。保存要求はドキュメントと同時にアクティブコンプを
  捕捉するので、キューされた保存はその時点のセッションを書く。
- 単位 3 ✅: REQ-UI-013 受入条件のうちツリー表示・選択連動・active 切替・
  非アクティブコンプ閲覧の項目を満たす。ツリー構築（分岐・共有ノード・
  未接続ノード・サブネット）はヘッドレスのユニットテストで固定する。
  実装メモ:
  - 「サブネットノードは葉」は**内側のネットワークを平坦化しない**という
    意味に限定した。サブネットノード自身の上流入力は外側ネットワークの
    ノードなので通常どおり展開する — 葉にすると `net.out` から到達可能な
    上流が Unused に落ち、グラフを誤って表示することになる。
  - 行の並び順は `Graph::inputs_of()` ではなく `edges()` を
    `(target, target_port, source)` で整列して決める。`inputs_of()` は
    順序なしのエッジマップを走査するので行順が安定しない（ノードエディタが
    入力を描く順＝ポート順に合わせた）。
  - 展開状態は「種別ごとの既定からの差分集合」1 つで持つ。既定はコンプ =
    開、レイヤー = 閉、ノード = 開、Unused = 閉。まだ存在しない行の既定も
    効くので、コンプ追加やレイヤー追加で状態を作り直す必要がない。
  - Node Editor の追従を `LayerSelection` の observe に移した。Timeline の
    push（`display_selected_layer_network`）は削除。`open_network` は
    「開く先を名指ししている `CanvasSelection`」を保持するようにし、
    Outliner がノード行選択で「まだ開いていないネットワークのノード」を
    選べるようにした。あわせて `notify_properties_selection` は選択が空の
    ときに `Layer` ターゲットを消さない（選択の書き手の所有物）。
  - ノード行のクリックは `LayerSelection` に加えて Node Editor へ
    `open_network` を明示的に呼ぶ。`LayerSelection` はサブネット深度を
    表さないので、同一レイヤーのサブネットに潜ったままだと選択ノードを
    含まないネットワークが開いたままになる。
  - コンプ行のシングルクリックは v1 ではパネルローカルなハイライトのみ
    （`PropertiesTarget::Composition` は単位 4）。レイヤー・ノード選択は
    共有 Global のみを正とする。
  - 実機確認（cliclick）: 3階層表示、分岐（Rasterize → In / RGB Color）、
    Unused グループ、レイヤー行シングル選択の Timeline 双方向一致、
    ノード行選択の Node Editor ハイライト + Properties 追従、レイヤー行
    ダブルの fit、ノード行ダブルのセンタリング。コンプ切替のダブルクリックは
    コンプを 2 つ作る手段が単位 4 なので自動テストで固定した。
- 単位 4 ✅: コンプの作成・複写・削除・設定編集がメニューと Outliner の
  両方から行え、それぞれ undo 1 回で戻る。コンプ 0 から新規作成できる。
  実装メモ:
  - コマンドの対象コンプは `panels::command_target_composition()` の 1 箇所で
    決める: Properties のコンポジションターゲット（= Outliner のコンプ行を
    選んだ状態）→ なければ active。これでメニュー・ヘッダーボタン・行の
    右クリックが**同一 Action** を投げたまま「ユーザーが指したコンプ」に
    効く。Outliner のコンプ行シングルクリックはパネルローカルな
    ハイライトをやめ、この共有ターゲットを書くようにした。
  - `manifest.json` の project 既定は初期値に使わない —
    `ProjectState` はロードしたマニフェストを保持しておらず、その既定値は
    フォールバック定数（1920×1080/30fps/300f）と同一。初期値は
    active コンプ → フォールバックの 2 段。
  - `CompositionSettings`（値の型）は `CommandId::CompositionSettings` から
    生成される GPUI Action と名前が衝突する。`workspace.rs` では
    `CompositionSettingsValue` として別名 import する。
  - gpui-component の素の `Dialog` は `button_props` を描かない
    （`AlertDialog` が footer を組み立てている）。OK/Cancel は
    `DialogFooter` + `Button` で自前に組む。削除確認だけは `AlertDialog`
    なので `button_props` が効く。
  - `Root` は view / tooltip / native menu overlay しか描かないので、
    ホスト側の render に `Root::render_dialog_layer()` と
    `render_notification_layer()` を子として置かないと**ダイアログは
    開いているのに見えない**。`RavelWorkspace::render` に追加した。
  - 削除後の active は隣（`neighbour_composition`）。undo はドキュメントを
    戻すが active は戻さない（コンプ切替は undo 履歴の外という単位 1 の設計）。
  - 実機確認（cliclick）: New ダイアログでの値入力と作成（active 切替 +
    Viewer のアスペクト追従）、コンプ行シングルで Properties にコンポジション
    セクション、ダブルで active 切替、Cmd+K で設定ダイアログ → リネームが
    Outliner と Properties に反映、複写（fresh レイヤー + 一意名 + active 切替）、
    レイヤーを持つコンプの削除確認 → 削除 → Cmd+Z で復活、空コンプは確認なしで削除。
    注意: gpui の `on_click` ボタンは cliclick の `c:` で押す
    （`dd:`/`du:` は `on_mouse_down` にしか届かない）。
- 単位 5 ✅: Outliner からレイヤーの並べ替え・削除・リネームができ、
  Timeline の表示順と常に一致する。
  実装メモ:
  - 並べ替えのヒット判定は行の `on_mouse_move`（行自身が index を知っている）
    で行い、座標計算をしない。ドロップ先がノード行や Unused 行でも
    「その行が属するレイヤー」に着地する。ドラッグ中は `apply_document`、
    mouse-up で 1 回 `commit_document`（Timeline のバー並べ替えと同規約）。
    パネル root の `on_mouse_up` で終了するので、行の外で離しても確定する。
  - レイヤー操作はアクティブコンプの行に限る。スタック順はドキュメント編集
    なので、表示していないコンプを暗黙に書き換えないため。
  - 右クリックメニューは Timeline のレイヤーメニューと同じく**パネルの
    メソッドを直接呼ぶ**（`EditDelete` などの Action は「フォーカス中の対象を
    削除する」意味なので、カーソル下の行に効かせる操作には使えない）。
  - ロック済みレイヤーは Rename / Delete が disabled。判定はドキュメントの
    値（行のミラーではなく）。
  - インラインリネームは blur / Enter で確定、Escape で取消。`InputState` は
    Escape のイベントを出さず、**Enter の action も行の購読には届かない**
    （既存の Properties 名前入力でも Enter は効かない = 本単位由来ではない）
    ので、行で `on_key_down` を扱う（`.agents/rules/gpui.md` がテキスト入力に
    認めている生キー処理。allowlist に理由付きで追記）。
  - 実機確認（cliclick）: 行ドラッグでの並べ替え（Timeline の順序と一致）、
    右クリック → Rename のインライン入力 → blur 確定、Duplicate（複製が
    元の直上に入り選択される）、Delete → Cmd+Z で復活。
    **Enter / Escape は自動化で確認できていない** — 合成 Return が GPUI に
    届かない（Cmd+K や Cmd+Z のような修飾付きコードは届く）。物理キーでの
    確認は手動で行う必要がある。
- 単位 6 前半 ✅ (#159): REQ-UI-013 の複数選択の受入条件（Shift 範囲 / Cmd トグルが
  Timeline と Outliner で同じ結果になり、Node Editor は 1 レイヤーのときだけ
  ネットワークを開き、Properties が選択数と共通値を出す）。
  実装メモ:
  - 修飾クリックの意味は `ravel-ui` の
    `panels::layer_selection`（`LayerClickMode::from_modifiers` /
    `layer_selection_after_click`）に純関数として置き、両パネルが同じ関数を
    通す。選択の並びは**アンカー先頭**（= `primary()`）: 範囲を繰り返し
    Shift クリックしたときにアンカーが動かず、伸縮が直感どおりになる。
    Cmd で足したレイヤーは新しいアンカーになる。
  - 範囲の起点が選択に無い / スタックに無いときは Replace に縮退する
    （選択が空のまま Shift クリックしても何も選べない状態を作らない）。
  - 修飾クリックはジェスチャを開始しない（`LayerClickMode::is_additive`）:
    Timeline のバー移動・トリムと、ヘッダー行 / Outliner 行の並べ替え D&D。
    選択を組み立てるだけのクリックでレイヤーが動くと取り返しがつかない。
  - 右クリックは既に選択に含まれる行なら選択を変えない
    （`select_layer_for_menu`）。後半の一括操作をカーソル下の行から呼べる
    ようにするため、単位 5 の「行の右クリックはパネルのメソッドを直接呼ぶ」
    規約と組み合わせる前提。
  - Node Editor は「0 個」と「複数個」を同じ閉じた状態にマップする。
    `close_network` が `CanvasSelection` をクリアするので、古いネットワークの
    ノード（と Viewer bbox）が残らない。メッセージだけ差し替えるため
    `follow_layer_selection` は無条件に `cx.notify()` する
    （context が既に `None` のときは `close_network` が早期 return する）。
  - Properties は `PropertiesTarget::Layers { comp_id, layer_ids }` を追加。
    `Layer` を複数対応にせず別バリアントにしたのは、編集経路
    （`apply_layer_change` / `toggle_key`）が単一レイヤーを前提にしており、
    複数を同じ型に混ぜると「編集できないターゲット」が編集経路に流れ込む
    ため。v1 は全フィールドが `ReadOnly`（選択数 + 共通値、相違は
    `MIXED_VALUE` の「—」）で、`route_change` も `Layers` を明示的に弾く
    （前のターゲットのウィジェットが生き残っていても書き込めない）。
    セクションの合成は `ravel-ui` の `sections_for_layers` で、比較は
    **表示テキスト**で行う（同じに見える値が相違扱いにならない）。
  - Properties ターゲットは選択から導出される状態なので、公開は
    `panels::publish_layer_properties_target` の 1 箇所に寄せた
    （`set_layer_selection` 自身に publish させると、コンプ切替時に
    `Composition` ターゲット = `command_target_composition` を消してしまう）。
  - レイヤー削除は選択からそのレイヤーだけ落とす（従来は選択ごと破棄）。
    複数選択の 1 行を消して残りが解除されるのは後半の一括削除と噛み合わない。
  - 実機確認（cliclick）: Timeline のヘッダー行 Shift 範囲 / Cmd トグル、
    バーの Shift クリックでバーが動かないこと、Outliner 行の Shift / Cmd、
    複数選択時の Node Editor 中央メッセージと Properties の「選択レイヤー」
    セクション、単一に戻したときネットワークが再び開くこと。
  - 既知の穴（前半では直さない）: 消滅したレイヤーを `LayerSelection` から
    落とす pruning は Timeline パネルの `sync_from_project` にしかない。
    Timeline を含まないワークスペース（`assets/workspaces/motion.toml` /
    `node.toml`）では undo などでドキュメントからレイヤーが消えても選択が
    古いまま残る（単位 1 以前からの挙動）。Properties 側は `Layers`
    ターゲットが 1 枚に縮んでも読み取り専用の複数表示を出し続けるので
    「編集できるように見えて効かない行」は出ないようにした
    （`sections_for_layers` は 1 要素でもマージ表示）。pruning 自体を
    `ProjectState` に移すのはパネル横断の変更なので別単位で扱う。
- 単位 6 後半 ✅ (#161): 複数同時移動が 1 undo であること、一括削除・一括フラグ・
  一括複写がそれぞれ 1 undo であること、レイヤー単位 bbox。
  実装メモ:
  - 一括編集のドキュメント関数は `ravel-ui` の
    `update_layers` / `remove_layers` / `duplicate_layers`。いずれも
    「1 呼び出し = 1 スナップショット」なので `commit_document` 1 回で
    選択全体が 1 undo になる。`remove_layers` はロック済みレイヤーを飛ばす
    （destructive 操作からの保護は単位 5 と同じ規約）。
  - 操作の対象は両パネル共通で `operation_targets(row)`:
    行が選択に含まれていれば選択全体、含まれていなければその行だけ。
    フラグの新しい値は**クリックした行**が決める（混在した選択が
    バラバラに反転せず一様になる）。
  - Timeline の S/M/L と展開三角は `cx.stop_propagation()` を入れた。
    行本体の on_mouse_down が選択を単一化してしまい、一括トグルの直後に
    選択が消える（＝続けて別のフラグを一括で切れない）ため。
  - Viewer のレイヤー bbox は「そのレイヤーのネットワークの shape ノードの
    bounds の和 → シェル変換」。shape ノードを持たないレイヤー（メディア・
    エフェクトのみ）は bbox を出さない。ハンドルは描かない — レイヤー選択に
    スケールのジェスチャは無いので、ハンドルは無い操作を示唆してしまう。
    描くのは「2 枚以上選択」のときだけで、これは**ドラッグで動く対象と
    完全に一致する**（1 枚選択のときはネットワークが開くので従来のノード
    bbox とノード選択が正）。
  - 複数同時ドラッグは `MoveDrag { targets: Vec<MoveTarget> }` に一般化した。
    レイヤーごとにローカルフレームが違う（REQ-LAYER-006）ので frame は
    target 単位。1 ドラッグの全 target を **1 つの Document** に適用してから
    `apply_document` するので、プレビューも undo も 1 回。
    シェル変換が単位行列でないレイヤーは対象から外す（comp 空間の delta を
    レイヤーローカルの `center_x/y` に書く方式の前提。既存のノード移動と
    同じ制限）。
  - 選択の pruning は `ProjectState::document_changed` に移した。Timeline
    パネルにしか無かったので、Timeline を含まないワークスペース
    （`motion` / `node`）では undo でレイヤーが消えても選択が古いままだった。
    Timeline に残っているのはコンプ切替時のクリアのみ。
  - 本単位で見つかった親チェーン変換の齟齬（Viewer が muted / 非-solo の親で
    連鎖を打ち切る一方、描画する `comp.transform` は全ての親を辿る）は解消済み。
    親子付けは可視性と独立（REQ-LAYER-001）と決め、行列計算を
    `ravel_core::composition::transform`（`world_matrix`）に一本化した。
  - 実機確認（cliclick）: 2 レイヤー選択でハンドル無しの bbox、その内側から
    ドラッグして両レイヤーの `center_x/y` が同じ delta だけ動くこと、
    Cmd+Z 1 回で両方戻ること、3 レイヤー選択で M を 1 回押すと 3 枚とも
    ミュート（選択は維持）、行の右クリック → Delete で 3 枚削除 → Cmd+Z 1 回で
    復活。**ポップアップメニューを閉じた直後の最初のキー入力は GPUI に
    届かない**（クリックを 1 回挟むと復帰する）ので、メニュー操作直後の
    Cmd+Z は手で確認する必要がある。

## 検証

- 各 PR で `mise run check`。ツリー構築・選択遷移・active 切替の
  不変条件（`LayerSelection.comp == ActiveComposition`）・`ui_state.json`
  の round-trip はヘッドレステスト（ravel-ui / ravel-app）を基本とし、
  D&D とダイアログ操作は実機確認（cliclick の dd/dm/du による CGEvent
  ドラッグ。System Events のクリックは GPUI に届かない）。
- 単位 1 は「挙動が変わっていないこと」が完了条件なので、既存テストの
  グリーンに加えて Timeline の選択 → Node Editor 追従、Viewer の bbox 表示、
  再生の duration を実機で確認する。
- 各単位で PR 前に `ravel-review` スキルを実行する。

## 非対象

- PreComp（コンポジション間参照）。`PathSegment::Comp` は予約のまま。
- 親子（parenting）の D&D 付け替え。並べ替え D&D とのジェスチャ設計を
  分けて行う。
- Outliner の検索・フィルタ欄、メディアアセットの Outliner 表示、
  タブグルーピング（`LayoutNode::Tabs` 未実装）。
- レンダー/エクスポート出力対象コンプの指定 UI（エクスポート導入時に
  `root_comp` の意味づけと合わせて設計する）。
- `ui-spec.md` に残る Sequence ノード糖衣モデル / Track・Clip 記述の全面
  改訂（Outliner 節のみ本計画で改訂した）。
