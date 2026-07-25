# Outliner + コンポジション管理実装計画（REQ-UI-013）

> **Status**: In progress — 単位 1〜5 完了、単位 6 未着手（2026-07-25 設計確定）

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
6. **複数選択の完成（次セッション）**: Shift 範囲 / Cmd トグルの選択 UI と、
   Node Editor の中央メッセージ・Properties の複数表示・Viewer の複数
   bbox（読み取り）までを前半、複数同時ドラッグ移動・一括削除・一括
   solo/mute/lock・一括複写を後半とする。複数同時ドラッグは
   REQ-UI-011 の `center_x/y` 再構築方式を複数レイヤー分まとめて
   1 undo にする必要があるため、単位 4/5 と混ぜない。

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
- 単位 6: REQ-UI-013 の複数選択関連と、複数同時移動が 1 undo であること。

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
