# UI 実装状況

各パネルの実装済み挙動・描画要素・未実装項目を記録する。

**この文書が実装状況の正**。「こう動くべき」という設計意図は
[`docs/specifications/ui-spec.md`](specifications/ui-spec.md)（索引）と
`docs/specifications/ui/<view>.md` にある。両方に同じ表を持たせない。

## Node Graph Editor (`panels/node_editor.rs`)

**ステータス**: TASK-014〜017 Done / layer-network Phase 3 Done
（ネットワークコンテキスト化）/ node-discoverability（ロケール化・ホバー
Popover・検索パレット・種別アイコン）Done

### 描画要素

| 要素 | 状態 | 詳細 |
|------|------|------|
| グリッドドット背景 | ✅ | ズームに応じてスペーシング変動、spacing < 5px で非表示 |
| ノード矩形 | ✅ | 角丸 6px、テーマ背景色 + ボーダー、ヘッダーラベル |
| ノードラベル・説明のロケール化 | ✅ | ラベル / 説明 / パラメータ説明は `node.<type_key>.*` ロケールキーから解決（組み込み 43 型すべて en / ja あり）。キー欠落は `type_key` フォールバック、ユーザーリネームは常に優先。Node Editor / Outliner / 追加メニュー / パレットは同一の解決経路（`ravel-app::node_locale`）。Properties の Node Info は `ravel-ui` が発行したキーを `read_only_value` が翻訳する経路で、登録済み型でキーが欠落した場合に限り生キーが出る（走査テストで到達不能） |
| ノードヘッダーアイコン | ✅ | 種別ごとのアイコンをカテゴリ色でキャンバスに直接描画（`paint_svg`）。サイズは 8〜32px の 5 段階に量子化（アトラス保護）、6px 未満では省略。未登録型はカテゴリ既定にフォールバック（カテゴリも不明なら汎用 NodeGraph）。ヘッダ高さ・ノード幅は不変 |
| ポートドット | ✅ | 入力=左端、出力=右端、DataTypeId ごとの Hsla カラーとシルエット（FrameBuffer=角丸四角、Geometry=菱形、Field=三角、Scene=六角、他=円）。色に頼れない場合も型の族が読み分けられる |
| ポートラベル | ✅ | 入力名=左寄せ、出力名=右寄せ |
| パラメータ表示 | ✅ | key: value 形式、セパレータ線付き |
| ベジェエッジ | ✅ | horizontal_bezier + 矢印付き |
| 選択ハイライト | ✅ | アクセント色ボーダー 2px |
| 接続ドラフト線 | ✅ | ポートドラッグ中に半透明アクセント色ベジェ |
| ビューポートカリング | ✅ | 画面外ノードはスキップ (50px マージン) |

### インタラクション

| 操作 | 状態 | 詳細 |
|------|------|------|
| パン (スクロール) | ✅ | マウスホイール dx/dy |
| パン (Alt+ドラッグ) | ✅ | Alt+左クリックでドラッグ |
| パン (中ボタンドラッグ) | ✅ | 中ボタンでドラッグ |
| ズーム (Cmd/Ctrl+スクロール) | ✅ | カーソル位置を中心にズーム |
| ノードクリック選択 | ✅ | Shift で追加選択 |
| ノードドラッグ移動 | ✅ | 選択中ノード全体を移動、im クレートで構造共有 |
| ポートドラッグ→接続作成 | ✅ | ポートヒットテスト → ドラフト線 → スナップ → エッジ追加 |
| 空白クリックで選択解除 | ✅ | |
| 矩形選択 | ✅ | Shift+ドラッグで矩形選択、AABB 交差判定 |
| エッジ選択 | ✅ | エッジクリックで選択 (ベジェヒットテスト 5px 閾値) |
| エッジ削除 | ✅ | Delete/Backspace で選択エッジ削除 |
| ノード削除 | ✅ | Delete/Backspace で選択ノード削除 (接続エッジも自動カスケード) |
| undo/redo | ✅ | UndoStack 統合、Cmd+Z / Cmd+Shift+Z / Cmd+Y |
| pinch ズーム | ✅ | トラックパッドピンチ |
| コンテキストメニュー (ノード追加) | ✅ | 右クリックで registry のテンプレート（`shape.custom_path` を除く）から追加。項目に種別アイコン付き |
| グリッドスナップ (ドラッグ中) | ✅ | 10px グリッドにスナップ（`node_editor.rs:2000`） |
| コンテキストメニュー (ノード削除) | ✅ | 右クリック → Delete Node |
| コンテキストメニュー (バイパス) | ✅ | 右クリック → Bypass Node (フラグトグル・チェック表示、評価器が入力をパススルー。Bypass 不可ノードでは無効化、Bypass 中は半透明描画) |
| コンテキストメニュー (ポート) | ✅ | ポート上で右クリック → Rename Port / Delete Port（network-interface-editing 計画 単位 4）。**項目はどのポートでも出し、編集できないポートでは無効化する**（固定ポート・通常ノードのポート・Subnet ノードのピン）。判定は `network::is_fixed_port` と In/Out 判定のみ（legacy `f` の例外もそこが持つ）。Rename は Outliner のレイヤー改名と同じ一回きりの `InputState` を、行ではなくポート位置に浮かせる（Enter / blur で確定、Escape で破棄）。Delete は Properties と同じ `remove_custom_port` 経路なので **1 操作 1 undo**（ポート・同名パラメータ・巻き添えのエッジが 1 スナップショット、残るポートのエッジは新しい index へ追随）。拒否された編集はキャンバス左下に理由を出し（Properties と同じ文言）、Rename は入力を開いたまま残す。Delete はメニュー構築時の名前を実行時に引き直し、名前が消えていれば何もしない（枠から推測して別のポートを消さない）。**ポート一覧が変わる編集（このパネル・Properties・undo/redo のいずれでも）は進行中のワイヤードラッグを取り消す** — `PortHit` は index 参照で、`add_edge` が index も型も検証しないため、ずれたままドロップすると誰も読まないエッジが黙って作られる |
| コンテキストメニュー (エッジスタイル切替) | ✅ | Edge Style → Bezier/Straight/Step |
| エッジスタイル描画 | ✅ | Bezier(S字), Straight(直線), Step(直角折れ線) + 各ヒットテスト。**設定に永続化される**（`settings.toml` の `[node_editor] edge_style`、既定 `bezier`）。コンテキストメニューでの変更は global 層へ書き戻し、パネルは起動時に解決済み設定から読むので、パネルを開き直しても再起動しても残る。設定ダイアログへの露出はまだ無い（`SET-*`）。`flow_direction` は上→下フローモード（`NGR-5`）と同時に入る |
| Copy/Paste (Cmd+C/V) | ✅ | ノード群+内部エッジをコピー、新IDでペースト |
| Duplicate (Cmd+D) | ✅ | 即時複製 (20,20) オフセット |
| ポート型フィルタリング | ✅ | 接続ドラッグ中に非互換ポートをスナップスキップ |
| 単一入力制約 | ✅ | 既存エッジを自動置換 |
| Fit View (F key) | ✅ | 全ノードが画面に収まるようズーム+パン |
| 自動整列 (L key) | ✅ | `CommandId::NodeAutoLayout`（`NodeEditor` キーコンテキストの `L`。パネル固有なのでユーザー再割り当ての対象外）。**選択が 2 個未満ならネットワーク全体**、2 個以上あればその集合だけを DAG の深さで層に分け、層は右へ、層の中は下へ並べる。層内の順は現在の Y 座標（同値ならノード ID）で決まり、結果は元のバウンディングボックス左上に揃うので選択の一部を整列しても飛ばない。synthetic ノードは動かさない。1 ノードだけの整列は並べる相手が無く定義上何もできないので、空選択と同じく全体へ倒れる — collapse 直後（新しい Subnet ノードが選択されたまま）に押しても重なりが解ける理由。**Document コミット 1 回**（`NodeMetadata::position` は保存データなので undo 1 回で全位置が戻る）。位置が 1 つも変わらないときは undo ステップを作らない。**自動では走らない** — collapse / extract / ノード追加のどの経路からも呼ばない（node-graph-readability 計画の決定事項） |
| Evaluator 連携 | ✅ | ProjectState の EvalService 経由（Document-aware、バックグラウンド） |
| ネットワークコンテキスト | ✅ | 所有パス（Comp/Layer/[Subnet...]）で 1 ネットワークを編集（REQ-LAYER-011）。`LayerSelection` を observe し、**レイヤー 1 つだけ選択中**のときそのネットワークを開く。0 個と複数個は同じ閉じた状態（中央メッセージのみ差し替え、閉じるとき `CanvasSelection` もクリア。REQ-UI-013 単位 6） |
| サブネットへの潜り | ✅ | サブネットノードをダブルクリックで内部 Graph へ |
| Subnet ノードの追加 | ✅ | Add Node / パレットから作った Subnet が内部 `net.in` / `net.out` ペアを持ち、そのまま評価でき（既定の出力ピンは `frame`）ダブルクリックで潜れる。外側ピンは内部 In / Out から導出され、内部のポートを追加・削除・改名・型変更・並び替えすると同じ Document コミットで外側ピンと外側エッジが追随する（**1 操作 1 undo**）。名前で対応付けるので並び替えで配線は保たれ、消えたピンのエッジだけが落ちる。ドリフトしたピンはロード時に修復される（内部グラフを持たない旧データは対象外） |
| Collapse / Extract | ✅ | 選択ノード群を Subnet にまとめる / Subnet を展開する（network-interface-editing 計画 単位 6）。コンテキストメニュー（対象は選択、無ければ右クリック先）と `CommandId::{NodeCollapseToSubnet, NodeExtractSubnet}`（Action 経路は選択に対して働く。既定のキーバインドは無し）の両方から同じパネルメソッドを呼び、**どちらも Document コミット 1 回**。Collapse は `network::can_collapse`、Extract は「Subnet ノード 1 つだけの選択」でメニューを先に無効化する。In / Out ノードと synthetic ノードは選択に含まれても対象外。ピンは境界エッジ 1 本ごとではなく**外側の端点ごとに 1 つ**（1 つの出力が複数ノードを駆動していても外側の配線は 1 本のまま）、名前は境界エッジの入力ポート名で、衝突は `_2` / `_3`。何も出ていかない選択には `frame` 出力ピンが 1 つ付く。ノード ID は移動しても変わらないのでラウンドトリップで接続関係と位置は戻る（エッジ ID は戻らない — 1 本が 2 本になり再び 1 本になるため）。Extract は内部 In の固定ポート（`t` / `f` / `base_geometry` / `source`）を親の In へ繋ぎ直し、外側が未接続だったカスタムピンは未接続へ落とす（promote パラメータの置き場が親に無いため）。**失敗時の UI 通知は無い**（メニューが先に無効化するので届くのはメニューを開いたままグラフが動いた場合だけ。`tracing::warn!` のみ） |
| パンくずバー | ✅ | Comp / Layer / Subnet... を表示、クリックで任意の深さへ戻る |
| synthetic ノード非表示 | ✅ | `NodeMetadata.synthetic` を描画・ヒットテスト両方でフィルタ |
| ノード処理時間表示 | ✅ | ノード下に評価時間（例 12ms）。8ms 以上で黄、33ms 以上で赤 |
| ポインタフィードバック | ✅ | ポート / 空白=`Crosshair`、ノード=`OpenHand`、エッジ=`PointingHand`。接続スナップ時は `DragLink`、移動 / パン中は `ClosedHand` |
| ノード検索パレット | ✅ | Tab（トグル、最後にキャンバス上にあったポインタ位置。一度も乗っていない / キャンバスが縮んで外に出たときのみ中央）/ 空所ダブルクリック（カーソル位置）/ ワイヤーを空所にドロップ（接続可能な型のみ候補）。ロケール解決後の label + description と `type_key` を大小無視の部分一致で検索し（`shape.rect` のような型名で引ける。候補行に `type_key` は出さない）、一致段は label > `type_key` > description の順、その同じ段の中で最近使用（セッション内メモリ、最大 10 件、永続化なし）が上位。カテゴリフィルタチップ、候補行にアイコン + ラベル + カテゴリ。↑↓/Enter/Escape は入力の capture フェーズで処理。閉じると状態は残らない。確定は右クリックメニューと同じ経路（1 undo）。**↑↓/Enter/Escape の実イベントテストは未**（絞り込み・発動・配線はテスト済み） |
| ホバー Popover | ✅ | ノード上に 500ms 滞留で詳細 Popover（gpui-component Popover 制御モード、フォーカスを奪わない、ゼロサイズ absolute wrapper アンカー）。ジェスチャー中（移動 / 接続 / 矩形選択 / パン）は抑制。内容: アイコン + ラベル / カテゴリ / 説明（無ければ節ごと省略）/ 入出力ポート（名前 + 型）/ パラメータ（名前 + 表示フレームの現在値 + 説明。カーブを持たないチャネルソースは 0 表示）。評価要求は出さない。**実機での目視確認は未** |
| Document 単位 undo | ✅ | ネットワーク編集は Document へ splice（replace_network）→ ProjectState commit。undo/redo はパネルでは処理せずワークスペース → Document undo |
| ミニマップ | 🔲 | 後続タスク |

### ファイル構成

| ファイル | 役割 |
|---------|------|
| `ravel-app/src/node_editor/viewport.rs` | Viewport 座標変換、ズーム、fit_to_content |
| `ravel-app/src/node_editor/bezier.rs` | ベジェ曲線計算、距離ヒットテスト |
| `ravel-app/src/node_editor/painting.rs` | canvas 描画関数群、ポートヒットテスト、スナップ検出、ヘッダーアイコン描画（サイズ量子化） |
| `ravel-app/src/node_editor/hover_popover.rs` | ホバー Popover（滞留の状態機械、内容モデル、gpui-component Popover 配線） |
| `ravel-app/src/node_editor/palette.rs` | ノード検索パレット（候補生成、ワイヤードロップの型フィルタ、オーバーレイ UI） |
| `ravel-app/src/node_editor/port_colors.rs` | DataTypeId → Hsla マッピング |
| `ravel-app/src/node_locale.rs` | `node.<type_key>.*` ロケールキーの解決（表示テキスト化はこの 1 箇所） |
| `ravel-ui/src/node_locale.rs` | ロケールキーの組み立てとユーザーリネーム判定（i18n 非依存の純粋層） |
| `ravel-ui/src/node_search.rs` | パレットの検索絞り込み・ランキング（純粋関数） |
| `ravel-app/src/assets.rs` | `RavelIcon::for_node_type` / `for_category`（種別・カテゴリ → アイコンの対応表） |
| `ravel-app/src/panels/node_editor.rs` | Panel 実装、DragMode 状態機械、イベントハンドラ、パレット発動・最近使用管理 |
| `ravel-core/src/registry/` | NodeRegistry + NodeTemplate + builtin テンプレート（40 型） |

### デモデータ

- なし。起動時はコンテキストなし（タイムラインからネットワークを開くまで
  ヒントを表示）。

---

## Properties Panel (`panels/properties.rs`)

**ステータス**: TASK-017 Done

### 描画要素

| 要素 | 状態 | 詳細 |
|------|------|------|
| Accordion セクション | ✅ | Node Info / Parameters をデフォルト展開 |
| ReadOnly フィールド | ✅ | key-value テキスト表示 (type, label, id) |
| ノード説明 (Node Info) | ✅ | ロケールの `node.<type_key>.description` を Node Info セクションに表示（無い型では出さない）。選択中ノードのアイコン表示は未実装（未計画） |
| Float/Int フィールド | ✅ | ラベル + ScrubInput（ドラッグスクラブ + クリックでテキスト編集） |
| Vector フィールド | ✅ | `Channel2` / `Channel3` パラメータを成分ごとの ScrubInput の横並び 1 行で表示・編集。値ドメインのベクタ定数（`constant.vec2` / `.vec3` / `.vec4`）と、組み込みノードのベクタパラメータ（`shape.*` の `center`、`shape.ellipse` の `radius`、`scatter.grid` の `spacing`、`geometry.transform` の `translate` / `rotation` / `scale` / `pivot`、`transform` の `translate`、`field.falloff` の `center` / `direction`、`scatter.scatter` の `area`、`type` が `vec2` / `vec3` の `attribute.set` の `value`）が到達する。成分ラベルとリンクトグルは未実装（MED-APP-20）。4 成分（`attribute.set` の `type = "vec4"`）は Color 描画のまま（MED-APP-19） |
| Curve フィールド | ✅ | `ParameterValue::Curve`（`field.curve_remap` の `points`）が到達。折り畳み時はカーブのサムネイル、行クリックで直下にインラインエディタを展開。複数行を同時に展開でき、展開高さはハンドルドラッグで変更。展開部はグリッド + 軸目盛（表示範囲から導出、短い軸ではラベルを間引く。表示範囲は f64 で保持し深いズームでも潰れない）、選択点の入力/出力の数値表示・編集、補間種別（Linear/Bezier/Step）の切替、ベジエ接線ドラッグ（Shift で 45 度スナップ）、表示範囲 min/max の数値編集、ホイールズーム、Fit（ベジエ接線ハンドルも可視域に含める）を持つ。展開状態・高さ・表示範囲・選択はビュー状態で Document に入らない（undo 対象外、ターゲット切替で展開はリセット） |
| Ramp フィールド | ✅ | `ParameterValue::Ramp`（`field.ramp` の `stops`）が到達。折り畳み時はグラデーションの帯、行クリックで直下にインラインエディタを展開。**展開の状態と高さはカーブ行と同じ 1 つの仕組み**なので、カーブ行とランプ行を同時に開ける。展開部は帯の上のストップマーカー（ドラッグで移動、空所のダブルクリックで追加、マーカーのダブルクリックで削除、クリックで選択）、選択ストップの位置の数値編集、補間種別（Linear/Smooth/Constant）の切替、選択ストップの色を編集する `ColorPicker`（Color 行と同じデバウンス commit）を持つ。位置は `0..=1` にクランプされ、ドラッグは隣接ストップを追い越さない。**最後の 1 個は削除できない**（`RampParam` は空になれない。1 個は単色として正当な状態）。展開状態・高さ・選択はビュー状態で Document に入らない（undo 対象外、ターゲット切替で展開はリセット） |
| Enum フィールド | ✅ | ラベル + 値表示 + Select ドロップダウン。**選択肢は保存値そのもの**（`Normal`、`2: pcm_s16le 44100 Hz 1 ch`）だが、データではなく状態を指す選択肢（Parent の `(none)` = `properties.value.none`）はロケールキーで発行され表示境界で翻訳される。Select は翻訳済みラベルを返すので、パネルは生の選択肢を並べて持ち書き戻す値を言語に依存させない（Ports の型メニューと同じ形） |
| Bool/String/Color | ✅ | key-value テキスト表示 (将来: 専用ウィジェット) |
| Ports セクション | ✅ | `net.in` / `net.out` 選択時のみ表示（network-interface-editing 計画 単位 3）。ノードが宣言する全ポートを 1 行 1 ポートで列挙し、**固定ポート（`net.in` の `base_geometry` / `t` / `f` / `source`、`net.out` の `frame`）は読み取り専用行**（名前と型のみ、ツールチップで組み込みと明示）。カスタム行は名前 Input・型 Select・上下移動・削除ボタンを持ち、末尾に追加行（名前 + 型 + `+`）。型 Select の選択肢は文脈依存（レイヤールートの In は値型 6 種、サブネット内 In は全 10 種、Out は 8 種 — `Int` / `Bool` は Out 側に種別の置き場が無く `Float` と区別できないので提示しない）。拒否された編集（重複名・予約名・許可されない型・空名）はセクション下に理由を表示 |
| 公開パラメータセクション | ✅ | `CommandId::ProjectExposedParameters`（Cmd+Shift+K / コンポジションメニュー）で開くプロジェクト対象のみに表示（exposed-parameters 計画 EXPO-5）。1 行 1 宣言で、名前 Input・型と既定値の読み取り専用表示・説明 Input・上下移動・削除ボタン。**型と既定値は編集できない**（公開した時点でパラメータから導出され、変えると `apply` が書き戻せない宣言になる）。**追加行は無い** — 宣言はパラメータ行の公開トグルから作る。束縛が届かない宣言には警告アイコンと理由（`BindingIssueReason` ごとに 1 ロケールキー）を行の下に表示。宣言ゼロのときはその旨を表示。拒否された編集（重複名・空名）はリスト下に理由を表示 |
| 式エディタ | ✅ | 式が付いた成分ごとに 1 行のテキスト入力を行の直下に展開。コンパイルエラーは入力欄の下に位置付き（行・列）で表示され、**確定を妨げない**（壊れたソースもそのまま保存され、値はチャネル既定値になる）。ソースは `ChannelSource::Expression` から毎リフレッシュ導出するので undo で巻き戻る |
| 空状態プレースホルダー | ✅ | ノード未選択時に表示 |

### インタラクション

| 操作 | 状態 | 詳細 |
|------|------|------|
| ノード選択連動 | ✅ | SelectedPropertiesTarget Global で自動切替 |
| レイヤー選択連動 | ✅ | Timeline / Outliner のレイヤー選択で Layer セクション表示・編集（殻属性: 時間配置/Transform/opacity/blend/adjustment、音声を持つレイヤーでは Audio セクションの gain/fade/audio mute、およびアセットが持つ音声ストリーム一覧からの選択（コンテナのストリーム番号 + codec/rate/ch。一覧は `AssetMetadata` 由来で probe しない）、ProjectState 経由で Document 更新）。複数選択時は読み取り専用の Layers ターゲット（選択数 + 共通値、相違は「—」。一括編集は後半） |
| In カスタムパラメータ | ✅ | `custom.<name>` フィールドとして表示・編集（REQ-LAYER-002）。編集は In ノードのパラメータへ書き戻し |
| Bool 編集（レイヤー） | ✅ | solo/muted/locked/adjustment を Checkbox で編集 |
| Parent（親レイヤー）の設定 | ✅ | Transform セクション先頭の Parent ドロップダウン（layer-shell-wiring 計画 単位 5、REQ-LAYER-001）。候補は同一コンプの他レイヤー + `(none)`。**自身と自身の子孫は列挙しない**ので UI から親子循環を作れない（評価側の停止保証は残っている）。選択肢は `{layer_id}: {名前}` なので同名レイヤーでも別の親を指す。付け替え・解除はどちらも `InvalidationHint::Structural` で **1 操作 1 undo**（`compile.rs` が親の Transform ノードから辺を張るため構造変化）。親レイヤーを削除すると子の `parent` は `None` に戻る（`Composition::remove_layer` が解決不能な `LayerId` を残さない）。Timeline の Parent 列・Outliner の D&D・Viewer の親子リンク線はこの単位の対象外 |
| スクラブでパラメータ変更 | ✅ | 感度=UI レンジ由来、clamp=hard レンジ。Shift=10x / Cmd=0.1x。NodeEditorHandle 経由の deferred direct call で Graph 更新。**式で駆動された成分はスクラブしても変わらない**（値の出どころは式なので、ドラッグ中に動いた表示は離すと戻る）。編集は行の下の式エディタで行う |
| クリックでテキスト入力 | ✅ | gpui-component Input（EntityInputHandler 経由）。全選択で開始、Enter/blur で確定・clamp、パース不能は復元。IME 実機確認は未 (#41) |
| Select でパラメータ変更 | ✅ | Enum パラメータ (merge operation、`attribute.set` の `type` 等)。`type` の変更は `value` のアリティも変え、露出済みパラメータポートの型を追随させる（合わなくなったエッジは破棄。値・ポート・エッジで 1 undo） |
| カスタムポートの編集 | ✅ | Ports セクションからの追加・改名・型変更・並び替え・削除。いずれも `NodeEditorHandle` 経由の deferred direct call → `commit_graph` で **1 操作 1 undo**（ポート・同名パラメータ・巻き添えのエッジが 1 スナップショット）。型変更はポートの index を保つ（新しい型を運べないエッジのみ破棄、パラメータは新しい型の既定値に置き換わる）。並び替えは固定ポートを跨がない。改名と削除はノードエディタのポート右クリックからも同じ経路で行える（単位 4、NodeEditor の表を参照） |
| undo/redo | ✅ | Document 単位 undo（ProjectState）。**undo 単位=ジェスチャ**（スクラブ中の Change は undo を積まず、ドラッグ終了の Commit で 1 スナップショット） |
| キーフレームトグル (◆/◇) | ✅ | アニメート可能フィールド左のダイヤボタンで現在フレームにキー追加/削除（1 undo）。殻 Transform/Opacity/Audio Gain・custom.*・ノード Float/Channel* 対象。定数 Float は Channel 化（REQ-LAYER-004） |
| 式トグル (Σ) | ✅ | チャネルを持てるノードパラメータ（`Float` / `Channel` / `Channel2-4`）の左、キーフレーム ◇ の隣。押すと**定数の成分にだけ**式が付き、**その成分はいま表示されている値を数値リテラルとして持つ**ので絵は動かない。キーフレーム・ノード出力・ブレンドで駆動された成分は上書きしない（戻り先が消えるため）。全成分がそうなっている行は Σ が無効表示になり tooltip が理由を出す。もう一度押すと外れ、そのとき表示されていた値の定数に戻る（式の付いていない成分のキーフレームは残る。式が `Blend` の中にある場合は **`Blend` を残したまま式の側だけ**定数化する）。入力欄は**打鍵ごとに構文エラーを更新**するが文書には書かず、確定は Enter と blur だけ（undo 後は入力欄が文書へ再同期し、古い内容が blur で書き戻ることはない）。1 操作 1 undo。**レイヤーの殻プロパティ（Transform / Opacity / Audio Gain / `custom.*`）には出ない** — 殻のチャネルも式を評価するが付け外しの UI が無い |
| 式駆動パラメータの値表示 | ✅ | 式で駆動された行はコンプの設定から作った `EvalContext` で評価した値を出す（`fps` / `res.*` / `comp.*` を読む式のため）。ノードグラフのノード本体は評価コンテキストを持たないので、そこでは数値ではなく**ソース文字列**が出る |
| アニメーションチャネル保持 | ✅ | キーフレーム付きチャネルのスクラブは平坦化せず現在フレームにキー挿入/更新（殻・custom.*・ノードパラメータ共通） |
| カーブ点の編集 | ✅ | インライン展開したカーブエディタで点をドラッグ移動、空所ダブルクリックで追加、点のダブルクリックで削除、クリックで選択。**両端 2 点は x 固定（y のみ編集可）** — 両端はカーブの定義域そのもの。定義域が変わるのは明示的な 2 操作だけ（定義域の外側への点の追加で広がる / 端の削除で縮む。ただし 2 点のときは削除不可）。選択点は数値でも編集可（非有限値は拒否して直前値に戻す）。**undo 単位=ジェスチャ**（ドラッグ中の Change は積まず、終了の Commit で 1 スナップショット。接線ドラッグ・数値編集も同じ）。展開・折り畳み・ズーム・Fit は値に影響せず undo にも積まない |
| パラメータの公開トグル (□/■) | ✅ | ノードのパラメータ行左の四角ボタンで、そのパラメータをプロジェクトの公開パラメータ宣言にする（REQ-PROJ-006、1 undo）。宣言名はパラメータキー、型と既定値は `exposed::apply::seed_value` が再生ヘッドのフレームでのパラメータ値から導く（アニメーション中の成分はそのフレームでの評価値。書き戻せないことは宣言リストが `AnimatedComponents` として出す）。外部契約にできないパラメータ（`PathPoints` / `Curve`、素材未設定のメディアノード）にはトグルを出さない。**押し戻しで取り消せる**（`MED-APP-26`）— 四角ボタンはチェックボックスとして描かれているので、宣言済みのパラメータをもう一度押すと宣言を取り下げる（同じく 1 undo）。トグルは**パラメータ単位**なので、そのパラメータに束縛された宣言が複数あれば（手書きの `.ravprj` で可能）全部まとめて外す。どちらの半分が走るかは描画時のフラグではなくドキュメントを読み直して決める。ツールチップは状態で出し分ける |
| 宣言の編集 | ✅ | 公開パラメータセクションで改名・説明の編集・並べ替え・削除（いずれも 1 操作 1 undo）。改名が既存の宣言名と衝突すると拒否して理由を表示し、リストは変わらない。何も変わらない操作（先頭行の「上へ」、同じ説明の再確定）は undo を積まない |
| 値ラベルリアルタイム更新 | ✅ | スクラブ中に値表示更新 |

### ファイル構成

| ファイル | 役割 |
|---------|------|
| `ravel-ui/src/properties/mod.rs` | PropertySection, PropertyField, PropertyValue 型定義 |
| `ravel-ui/src/properties/node.rs` | ノード用セクション生成 (NodeInfo, Parameters, Ports) |
| `ravel-ui/src/properties/layer.rs` | レイヤー用セクション生成 (Layer, Transform, Timing, Compositing) |
| `ravel-ui/src/properties/exposed.rs` | 公開パラメータ宣言のセクション生成（行・既定値の表示形・解決不能理由のロケールキー） |
| `ravel-app/src/panels/properties.rs` | PropertiesGpuiPanel (GPUI描画、ウィジェット管理) |
| `ravel-app/src/widgets/scrub_input.rs` | ScrubInput（スクラブ + テキスト編集の数値ウィジェット） |
| `ravel-app/src/widgets/param_curve_editor.rs` | ParamCurveEditor（`CurveParam` のインラインエディタ。座標変換と接線スナップは `widgets/curve_editor.rs` と共有） |
| `ravel-app/src/widgets/param_ramp_editor.rs` | ParamRampEditor（`RampParam` のインラインエディタ。ドラッグのクランプ規則を `param_curve_editor` と共有。色は `ColorPicker` がパネル側にあるため状態だけ持つ） |
| `ravel-app/src/widgets/curve_view.rs` | CurveValueRange（表示範囲のビュー状態）と目盛の刻み。Timeline のグラフエディタと Properties が共有 |
| `ravel-app/src/panels/mod.rs` | PropertiesTarget, NodeEditorHandle |

---

## Timeline (`panels/timeline.rs`)

**ステータス**: Document 駆動（layer-network Phase 3）

旧 Track/Clip モデルは廃止済み。現行タイムラインは Document の root
Composition を表示・編集し、レイヤー編集は Document 単位 undo に統合。

### 描画要素

| 要素 | 状態 | 詳細 |
|------|------|------|
| ルーラー | ✅ | 高さ 24px、MM:SS:FF 形式、ズームに応じたティック間隔適応 |
| レイヤーヘッダー | ✅ | 幅 200px、展開矢印、名前、S/M/L トグルボタン |
| レイヤーバー | ✅ | 角丸 4px、start_frame/duration 反映、名前テキスト |
| プロパティ展開行 | ✅ | 殻の AnchorPoint/Position/Scale/Rotation/Opacity（AE の並び。音声を持つレイヤーは Gain も）+ キーフレームを持つネットワーク内パラメータ（In カスタム・サブネット露出含む、REQ-LAYER-004） |
| チャンネル行の値 | ✅ | 行の右端に再生ヘッド時点の値を出す（`ScrubInput`）。単位は Properties と同じでスケール・不透明度はパーセント、ネットワークパラメータは生値。式・ブレンド・ノード出力に駆動された成分には出さない |
| キーフレームダイヤ | ✅ | Keyframes チャンネルをレイヤーローカル→Comp 時間へ変換して描画（`comp_frame_for_key`、in_frame 考慮）。選択中は描き分け |
| 再生ヘッド | ✅ | 赤色 2px 縦線 |
| コンポ終端の帯 | ✅ | Duration の外をルーラーとレーンで落とす（背景ウォッシュ + tint + 終端 1px 線）。ズームは終端で止めない。グラフモードには敷かない |
| BPM グリッド | ✅ | フレームグリッドと独立で同時表示。拍 = `offset + n × (fps × 60 / BPM)` をフレームに丸めずに描く。間隔 4px 未満なら描かない。テンポは 1〜999 に丸め込み。状態は `ui_state.json`（`BpmGrid`）で、保存したプロジェクトに残る |
| ループ範囲 | ✅ | コンポジション単位のイン / アウト（`B` / `N` / `Alt+B`、ルーラーの `Alt`+ドラッグ）。ルーラーに帯を描き、終了点を再生してから折り返す。範囲外へのシークでループが外れる。音声はミキサーの読み出し位置を折り返すので折り返しで途切れない。状態は `ui_state.json`（`LoopRange`） |
| キャッシュ帯 | ✅ | ルーラー下端に 3px の緑の帯で、出力段フレームキャッシュが持つフレームを描く（`CACHE-6`）。範囲は `cached_ranges(comp, &EvalContext)` そのままで、**ビューアが次に投げる文脈と同じ**もの（解像度・精度・品質・fps・コンプ解像度の全軸）で引く。再生・スクラブで伸び、手が止まっている間は先読み（`CACHE-9`）が前方を埋めた分も伸びる。編集で**その場で**消えるが、消えるのはノードを名指しした編集ならその所有レイヤーの時間範囲だけ（`CACHE-7`。評価完了を待たない。空コンプ・コンパイルエラーの経路でも消す）。サブフレーム位置は帯にしない。ディスク層（青）は層が未実装。更新は範囲が変わったときだけグローバルへ書き（比較は map を clone する前）、キャッシュヒットだけの評価では `version()` を見て全走査自体を飛ばす。パネルは購読せず再描画のついでに読む（`HIGH-21` を作らない） |
| タイムコード表示 | ✅ | ヘッダー左上コーナーに M:SS:FF（再生ヘッド位置、固定幅表示） |
| 選択ハイライト | ✅ | レイヤーヘッダー背景色変更 |

### インタラクション

| 操作 | 状態 | 詳細 |
|------|------|------|
| 再生ヘッド移動 (ルーラークリック) | ✅ | クリック位置のフレームに移動 + PlaybackClock を seek |
| 再生ヘッドスクラブ (ルーラードラッグ) | ✅ | ドラッグで連続追従 + PlaybackClock を seek（再生位置・評価フレームに反映） |
| キーフレームへのスナップ | ✅ | ルーラーの押下・ドラッグ中に `Shift` を押していると、**画面に出ているキーフレーム**へ 8px のピクセル半径で吸着する（ズームに依らず同じ操作感）。候補は `TimelinePanel::visible_keyframe_frames` が `visible_property_rows` から作るので、畳んだレイヤーのキーにも絞り込みで消えている行のキーにも吸われない。コンプフレーム 0 より前のキーは候補外 |
| 水平スクロール | ✅ | マウスホイール dx、scroll_offset 更新 |
| 垂直スクロール | ✅ | レイヤーリスト領域 overflow_y_scroll |
| ズーム (Cmd/Ctrl+スクロール) | ✅ | カーソル位置アンカー、pixels_per_frame [0.1, 50.0] |
| レイヤー選択 (ヘッダー/バークリック) | ✅ | `LayerSelection` Global へ書き込み → Properties / ノードエディタが observe。Shift で範囲選択、Cmd（platform 修飾）でトグル（REQ-UI-013 単位 6、修飾クリックはバー移動・並べ替えを開始しない）。選択中の全レイヤーをハイライト。削除・複写・S/M/L・バーの移動 / トリムは選択全体に効く（各 1 undo、ロック済みは削除と移動から保護。S/M/L は行本体の選択を奪わない） |
| ネットワークを開く | ✅ | レイヤーを 1 つ選択するとノードエディタが `LayerSelection` を observe して開く。ダブルクリック（ヘッダー/バー）は加えてビューを fit する。0 個・複数個選択時は閉じた状態 |
| レイヤー展開 (▶/▼) | ✅ | プロパティグループ・チャンネル行の開閉 |
| プロパティ行の絞り込み | ✅ | AE の reveal 一式（`U` / `A` / `P` / `S` / `R` / `T` / `L`、`Alt+U` = 変更済み、`Alt+E` = 式を持つ行）。**修飾なしは置換、`Shift` 併用は追加**、同じキーの二度目で全表示に戻る。行の生成は変えずフィルタを 1 枚かぶせるだけで、描画・ヒットテスト・ラバーバンド・高さ計算のすべてが `TimelinePanel::visible_property_rows` を通る。絞り込みはパネル状態なのでレイヤーを選び直しても保たれ、`ui_state.json` には載らない（起動時は常に全表示）。展開状態は変えない |
| Solo/Mute/Lock トグル | ✅ | Document 更新（solo/mute は Structural 再評価） |
| レイヤー作成 | ✅ | Layer メニュー（Solid/Shape/Video/Null、テンプレートから生成） |
| レイヤー削除 | ✅ | Delete/Backspace（locked は保護）、Document undo で復元 |
| レイヤー複製 | ✅ | Cmd+D、または行の右クリック → Duplicate。複数選択なら選択全体を 1 undo で複製し、コピーは各元の直上に入る |
| Document/undo 統合 | ✅ | 追加・削除・並べ替え・トリム・移動すべて Document 単位 undo |
| レイヤーバードラッグ移動 | ✅ | バー本体ドラッグ = start_frame 移動、端 6px = in/out トリム。1 ジェスチャ = 1 undo。**移動もトリムも選択全体に効く**（`MED-APP-28`。ロック済みは対象外、トリムの制限はレイヤーごとに自分の表示区間で掛かる）。選択済みバーの修飾なし押下は選択を保ち、動かさずに離したときだけ 1 枚へ絞る |
| レイヤー並べ替え | ✅ | ヘッダー縦ドラッグ |
| ポインタフィードバック | ✅ | ルーラー / トリム端、バー、ロック、キー / グラフアンカー / 接線を既存ヒット境界で区別。ドラッグ中も操作別カーソルを維持 |
| キーフレーム選択・移動 | ✅ | ダイヤクリックで選択+ドラッグ移動（live apply、mouse-up で 1 undo）。空所クリックで選択解除 |
| チャンネル値のスクラブ | ✅ | チャンネル行の値をドラッグして変更（Properties と同じ `ScrubInput` の挙動、クリックで数値入力）。**1 ジェスチャ 1 undo**（ドラッグ中は `apply_document`、終端で `commit_document`）。書き込みは `keyframes::set_channel_value` の 1 経路で、キーのある成分は**ジェスチャ開始時に固定した**レイヤーローカルフレームのキーを更新、定数の成分は定数を置換。ジェスチャの終了は 1 経路に集約（行がツリーから消えた・パネルが他をコミットする直前・パネル破棄）。値が動いていないジェスチャは `Commit` を飛ばさないので、コミット可否の判断もウィジェットと確保フレームの破棄もそこで行う。パネル破棄時はライブの値をコミットする（`HIGH-28` と同型の穴を作らない）。ロックの判定は開始時のみで、Timeline のロック操作は先に進行中のスクラブを確定させる。ウィジェットの生成・破棄は文書同期・再生ヘッド移動・展開・絞り込みの各経路で、`render` は読むだけ |
| レイヤーの分割 | ✅ | `Cmd+Shift+D`（Timeline のキーコンテキスト。focus 中は `panel.detach` を覆う）。プレイヘッドで選択レイヤーを 2 枚に割り、後半は真上に入る。写像 `comp - start_frame + in_frame` が両半分で同じなのでキーフレーム・ネットワーク・殻のチャネルは書き換えず、**2 枚が元の時間範囲を過不足なく覆う**。選択全体で 1 undo、後半が選択される。プレイヘッドが内側に厳密に入っていないレイヤーとロック済みは対象外 |
| 始端 / 終端をプレイヘッドへ | ✅ | `[` / `]` で選択レイヤーを尺を変えずに動かす（`]` は半開区間の終端を合わせるので次を `[` で繋ぐと隙間ができない）。ロック済みは動かさない（判定は Document 側。パネルの鏡は 1 flush 遅れる）。選択全体で 1 undo |
| 始端 / 終端へ移動 | ✅ | `I` / `O` で選択の最も早い始端 / 最も遅い終端へプレイヘッドを飛ばす。Document を触らないので undo に積まず、進行中の値スクラブも止めない |
| キーフレーム追加 | ✅ | チャンネル行の空所ダブルクリックでそのフレームに追加（現在値、1 undo） |
| キーフレーム削除 | ✅ | ダイヤ選択中の Delete/Backspace はキーフレームのみ削除（未選択時は従来通りレイヤー削除）。locked 保護あり |
| 再生/停止連携 | ✅ | PlaybackController（Space/K/←/→、メニュー）が playhead を駆動。follow トグル（コーナーの F）で表示範囲がページ追従。停止の着地点は設定（`settings.toml` の `[playback] stop_returns_to_play_start`、既定 `false` = 従来どおりフレーム 0 へ巻き戻し）。`true` ならその再生を開始したフレームへ戻り、一度も再生していなければ再生ヘッドは動かない。環境設定 ▸ 一般 の switch で切り替えられる |

### ファイル構成

| ファイル | 役割 |
|---------|------|
| `ravel-app/src/panels/timeline.rs` | GPUI Panel 実装、canvas 描画、イベントハンドラ |
| `ravel-ui/src/panels/timeline.rs` | ヘッドレス状態 (playhead, scroll, zoom, 選択, 展開, S/M/L) |
| `ravel-core/src/composition/` | Composition, Layer（殻+ネットワーク）, 殻コンパイル |
| `ravel-app/src/playback.rs` | PlaybackController（Transport + tick ループ、評価要求投函） |
| `ravel-core/src/runtime/playback.rs` | PlaybackClock（フレーム精度、wall-clock マスター） |

### デモデータ

- なし。起動時は空の root Composition（"Comp 1"、1920x1080、30fps、300f）。
  レイヤーは Layer メニューのテンプレートコマンドで作成する。

### 既知の制約

- Enum の選択肢値（ブレンドモード等）は識別子を兼ねるため未翻訳
  （セクション名・フィールドラベルは locale 経由）。
- タイムラインのプロパティツリーはサブネット内部まで再帰列挙する
  （深さ無制限。行ラベルは `Outer / Inner / blur · radius` のように
  サブネット名で修飾）。ただしサブネットの inner In が持つ
  カスタムパラメータのうち、外側の subnet ノードが promote している
  キーは列挙しない — 評価時に外側の値で覆われるため、行を出しても
  編集が効かない。

---

## Viewer パネル

`crates/ravel-app/src/panels/viewer.rs`

| 項目 | 状態 | 備考 |
|------|------|------|
| FrameBuffer 表示 | ✅ | `ViewerFrame` Global 経由。**macOS は GPU テクスチャを GPUI の surface としてそのまま描き**（`ZC-2`〜`ZC-4`）、Linux / Windows も wgpu surface 経路（`ZC-7` / `ZC-8`）でフレームを CPU に降ろさず描く — いずれもリードバック 0 回。テクスチャは GPUI の描画完了通知でプールへ返る。共有デバイスが取れない場合（複数 GPU 機など）は従来どおり `RenderImage` を `img` 要素で描く CPU 経路にフォールバックする。デバイス喪失はセッションで 1 度だけ通知して再起動を促す。どちらの経路でも表示バイト列は GPU で出来上がっている（`DisplayFrame`） |
| 表示変換（リニア → sRGB） | ✅ | 評価バッファはリニア光なので、リードバック前に GPU で 1 パス掛ける（`ravel_nodes::DisplayTransform`、`CM-7`）。**変換点はこの 1 箇所**で、CPU 側に画素ごとの色計算は無い。`scripts/lint-patterns.sh` の `raw-pixel-quantisation` が自前の量子化を禁じる。`quality` / `ViewerResolution` と直交。CPU の `to_display_rgba8` との一致基準は **8bit で ±1 コード**。ユーザー提供の `.cube` を差し込めるが**選ぶ UI は `CM-8`**、`.ocio` は `CM-9`（表示色空間は **sRGB 固定**）。変換が走らなかったフレームは Viewer のエラーオーバーレイになる（CPU 救済はしない）|
| root comp 常時評価 | ✅ | ProjectState が Document 変更・再生位置ごとに root comp 出力（殻コンパイル + Document-aware 評価）を要求（REQ-LAYER-007）。選択ノードの単独プレビューは不採用（ユーザー判断で削除） |
| Geometry 自動ラスタライズ | ✅ | 評価ワーカーの `GpuEvalHooks::finalize` で CPU reference により rasterize（GPU texture Viewer は後続） |
| 画像インスタンスの描画 | ✅ | `geometry.from_image` で包んだ FrameBuffer を `scatter.*` / `geometry.repeat` などで並べた結果が Viewer に出る。CPU 参照経路と GPU 経路の両方が描き、`source_index` でコピーごとに別の画像を選べる。**GPU 経路は GPU 常駐フレームを読み戻さない**（CPU 参照経路だけがソースごとに 1 回読み戻す）。コピーを拡大するとボケるのは仕様（`image-instancing-plan.md` 決定 1 / 5）|
| コンプ背景と透過確認 | ✅ | `Composition.background_color` は `comp.background` として評価結果へ合成。表示下地をコンプ背景 / 固定セルのチェッカーボード / 黒単色からセッション内で切替 |
| 未選択時プレースホルダ | ✅ | `viewer.no_output` locale キー |
| 再生・スクラブ・タイム同期 | ✅ | PlaybackController が再生/シーク毎に ProjectState へ root comp 評価を要求（latest-wins、ドロップ数カウント）。音声同期も実装済み: 音声トラックあり + デバイス稼働時は `SyncClock` が再生位置の正（`ClockSource::Audio`）、それ以外は従来の wall clock（`audio-plan.md` 単位 3） |
| GPU テクスチャ共有（ゼロコピー） | ⚠️ | **macOS / Linux / Windows で実装済み**（`ZC-2`〜`ZC-5`、`ZC-7`、`ZC-8`）。macOS は Metal、Linux / Windows は wgpu の出力テクスチャを surface としてそのまま描き、完了通知の後にプールへ返す。Windows は Ravel が GPUI の wgpu/DX12 renderer feature を有効化する。Linux / Windows の実機確認は未了。共有できない場合や context を取得できない場合は評価ワーカーで 1 回読み戻し → `RenderImage`（BGRA u8）の従来経路へフォールバックする。**デバイス喪失は別扱い**: surface 描画は採用したデバイスとレンダラの現デバイスを毎回照合して止まり、評価パイプラインは死んだデバイスに残るので復帰しないが、ユーザーへはセッションで 1 度だけ通知する（`HIGH-33`、GPULOSS-2 以降で復旧予定） |
| ツールバー（選択/ペン等） | ✅ | 選択 / ペン / 矩形 / 楕円 / ハンド / ズーム（`ToolState` Global、REQ-UI-011、`tool-system-plan.md`） |
| 選択 bbox とハンドル | ⚠️ | ノード選択（`CanvasSelection`）は評価済みジオメトリから bbox を描く。8 個の印は飾りのままで、ノードのサイズを書く操作は `ParamRole` のマニピュレータが担う。**レイヤー選択が 2 枚以上のときはレイヤー単位 bbox**（そのネットワークの shape ノードの bounds の和 → シェル変換、ハンドル無し。shape ノードを持たないレイヤーは出さない）。ジオメトリを置かないレイヤー（メディア / エフェクトのみ）は bbox 自体が出ない |
| レイヤー殻のマニピュレータ | ✅ | **Select ツールで、ジオメトリを持つレイヤーを 1 枚だけ選んでいるとき**、`ShellManipulator` が殻 bbox に自前の 8 個のスケールハンドル（角は 2 軸、辺の中点は 1 軸）、角の周りに回転リング（描画・当たり判定とも 18px 固定）、中央の移動グリップ、アンカーマーカーを出す。ノード選択 bbox の 8 個の印は飾りで、位置は一致するが別の印である。描画 / ナビゲーションツール中はオーバーレイごと引っ込むので、矩形・楕円・ペン・ハンド・ズームの押下を奪わない。**Shift はスケールだけ縦横比を固定し、Alt はアンカー基準**（回転の角度スナップと移動の軸拘束は未実装）。アンカーのドラッグは `anchor_point` と `position` を同時に書くので見た目が動かない。キーフレーム付きチャネルは平坦化せず、そのレイヤーのローカルフレームにキーを打ち、親を持つレイヤーは親チェーンの行列込みで表示・編集する。**1 ドラッグ = 1 undo、Esc で revert**。ドラッグ中にレイヤー選択が変わったら押下時のスナップショットへ戻して打ち切る。HUD はポインタ追従ではなくキャンバス左上に固定し、スケールはこのドラッグが掛けた倍率、回転は掃いた角度、位置とアンカーは座標を表示する。ジオメトリを置かないレイヤー（メディア / エフェクトのみ）は bbox とマニピュレータが出ない |
| 親子リンク線 | ✅ | 親を持つレイヤーを 1 枚選ぶと、子のアンカーから親のアンカーへ線を引く（OVL-7）。親の**設定**は Properties の Parent ドロップダウン（`SHELL-5`） |
| 複数レイヤーの同時ドラッグ | ✅ | レイヤー bbox の内側からドラッグで選択レイヤー全体を移動。`center` ベクタ再構築方式（REQ-UI-011）を全 target 分 1 つの Document に適用 → 1 undo。シェル変換が単位行列でないレイヤーは対象外 |
| ノードパラメータのマニピュレータ | ✅ | `ParamRole` は `Position` / `Size` の 2 種だけ。選択ノードの評価済みジオメトリにハンドルを出し、アニメーション付きでも現在のローカルフレームへ書く。接続で駆動されるパラメータにはハンドルを出さない。`Direction` / `Angle` は未導入 |
| ジオメトリ属性オーバーレイ | ✅ | 評価済みジオメトリの bbox / points / instances / paths と属性の矢印・インデックス・グループを表示。矢印は実際の `Vec2` / `Vec3` / `Vec4` 属性から作り、picker で選ぶ。ラベルは最大 64 件、矢印の長さ上限はコンプ座標の短辺 15% なので、殻スケールが大きいレイヤーでは画面上で伸びる |
| Field オーバーレイ | ✅ | 選択した field ノードを対象に、heatmap / 等値線 / ベクトル矢印を表示。等値線は marching squares ではなくセル解像度の階段。ツールバーから表示モード、カラーマップ、opacity を切り替える |
| モーションパス | ✅ | トグルは無く、Select ツールでレイヤーを 1 枚だけ選び、`position` に 2 個以上のキーがあるときに表示。サンプリングした折れ線とキー点を描き、キー点のドラッグはそのローカルキーフレームの両成分を書き換える。空間 Bezier は未実装 |
| ドラッグ中の吸着 | ✅ | レイヤー移動・シェイプ描画・殻のグリップが、コンプ枠の端と中心 / セーフエリア（表示中のみ）/ ユーザーガイド（表示中のみ）/ 動かしていないレイヤーの bbox の端と中心へ吸着する（`SNAP-1`、`SNAP-2`）。判定は**スクリーン 8px** で、コンプ空間へ逆変換して比較するのでズームを変えても効き幅は画面上で一定。同距離はコンプ枠 → セーフエリア → ガイド → レイヤーの列挙順で決まり、同じ入力なら毎回同じ候補を選ぶ。吸着した軸にはそのフレームだけマゼンタの線を描き、ジェスチャーが終われば消える。**Cmd / Ctrl 押下中は吸着しない**（Alt は中心描画・アンカー基準スケールで埋まっているため使わない）。**Shift が拘束として効く 2 経路（シェイプ描画・殻のスケール）では吸着しない** — 拘束が吸着後の座標を上書きしてガイドが嘘をつくため。編集しない軸には補正もガイド線も出さない。1 ドラッグ 1 undo の規約は変わらない |
| 定規とユーザーガイド | ✅ | キャンバス上端・左端の 16px の帯（既定は非表示、ツールバーのメニューで切替）。目盛りは 1-2-5 系列で、画面上の間隔が 6px 以上 15px 未満に収まるようズームに追従し、5 本ごとに長い目盛りを描く。**数値ラベルは無い**。上の帯から横ガイド、左の帯から縦ガイドをドラッグで作り、線から 6px 以内の押下で掴んで動かす。帯の上で離すと削除、メニューに「ガイドを消去」もある。**1 操作 1 undo**、`Escape` で revert、動かさずに離せば何もコミットしない。位置は `Composition.guides` が持ち、`.ravprj` の**版は上げていない**（`#[serde(default)]` の追加フィールド）。表示 / 非表示とロックは**セッション状態**でプロジェクトには保存しない。**非表示のガイドは描かれず吸着候補にもならない**が、**ロック中のガイドは描かれ吸着候補にも残る**（動かせないだけ）。操作は Select ツール限定 |
| ポインタフィードバック | ✅ | 描画、選択本体、パスアンカー / 接線、ペン閉路位置、**殻のスケール（4 方向の `Resize*`）/ 回転 / アンカー**、定規の帯とガイド線（`ResizeLeftRight` / `ResizeUpDown`）を区別。パン / 移動 / 描画 / 殻のドラッグ中もカーソルを維持。未実装の Hand / Zoom と、動作を持たないノード bbox のハンドルには割り当てない。GPUI-CE に回転カーソルが無いため `DragLink` で代用 |

評価はバックグラウンドワーカー（root comp は Composition 解像度）。
フレームは共有 `PlaybackPosition`（再生ヘッド位置）に従い、編集中も
一時停止中のフレームを再評価する。latest-wins でスクラブ Change・
再生フレームを間引き、UI スレッドは要求投函のみ。再生位置のクロックは
音声トラックあり + デバイス稼働時はオーディオデバイス（`SyncClock`）が
マスター、それ以外は wall clock にフォールバック
（`docs/implementation/audio-plan.md` 単位 3）。

### Viewer オーバーレイの制約

- 殻の移動は bbox 全面を `OverlayHandle` に載せ替えていない。レイヤー内ノードの
  クリック選択を潰すため、動作する移動グリップは bbox 中央だけ。複数レイヤーの
  既存内側ドラッグ移動は別経路で残る。
- ノード選択 bbox の 8 印は飾りで、`ShellManipulator` が同じ位置に自前の印を出す。
  見た目は一致しても、ノード bbox の印から shell の scale / rotation は書けない。
- ドラッグ HUD はキャンバス左上固定で、Shift はスケールだけに効く。回転の角度
  スナップと移動の軸拘束は無い。
- ジオメトリを置かないメディア / エフェクトのみのレイヤーには bbox が出ず、評価
  結果が未到着のときも推測した値は描かない。
- 属性矢印の長さ上限はコンプ座標基準で、等値線はセル解像度の階段である。
- `ParamRole` は `Position` / `Size` だけで、`Direction` / `Angle` は未導入。
  接続で駆動されるパラメータにはハンドルを出さない。
- **定規はオーバーレイではない。** `OverlayPainter` が知るのはコンプ矩形だけで、
  ズームインすると矩形はパネルから出る。定規はパネルの縁に貼り付くので、
  チェッカーボードと同じ `(panel, frame)` の組からキャンバスの描画クロージャで
  描く。定規の帯とガイド線の掴みも `OverlayHandle` ではなくパネル側の押下分岐が
  受ける（帯はコンプ矩形の外、ガイドは線なので「点からの半径」ヒットテストで
  表現できない）。`Select` ツール限定という条件で描画ツールの押下は奪わない。
- 吸着の閾値 8px と抑制キー（Cmd / Ctrl）に設定項目は無い。トグルを先に作らず、
  必要になってから追加する（`settings-screen-plan.md` の規約）。ピクセル
  グリッドへの吸着、回転の角度スナップ、ノードエディタの吸着ガイドは対象外。
- ガイドの表示 / 非表示とロックはセッション状態で、プロジェクトに保存されない
  （保存されるのは位置だけ）。定規に数値ラベルは無い。

---

## プロジェクト永続化（File メニュー）

**ステータス**: `.ravprj` フォーマット v8

| 項目 | 状態 | 備考 |
|------|------|------|
| New / Open / Save / Save As | ✅ | File メニュー配線済み。Save As/Open は GPUI ネイティブダイアログ。未保存時の Save は Save As にフォールスルー。dirty な New/Open は保存確認後に続行 |
| メディアインポート | ✅ | File ▸ Import…（`CommandId::FileImport`、Cmd+I、複数選択）と OS からのファイル D&D（REQ-UI-010）。probe は background executor、成功分だけ `media_assets` に相対化して登録するだけで、レイヤーは作らない（配置は別操作）。バッチ全体で 1 undo。同じ絶対パスは既存アセットを再利用 |
| UI 状態の保存 | ✅ | `ui_state.json`（アクティブコンプ、Timeline の BPM グリッド、コンポジションごとのループ範囲）。任意エントリで、欠落時はそれぞれ `root_comp` フォールバックと `BpmGrid` の既定、ループ範囲なし。既定のままの BPM グリッドとループ範囲ゼロ件はエントリ自体を書かない。読み込み時に無いコンプの範囲は捨て、Duration の外は引き戻す。既存 v3 アーカイブと互換（format_version 据え置き、REQ-UI-013） |
| ワークスペースレイアウトの埋込 | ✅ | 任意エントリ `workspace_layout.toml`。**オプトイン（既定 OFF）**で、OFF のときは書かれない（format_version 据え置き）。詳細は下の[ワークスペース節](#ワークスペースドッキングウィンドウ) |
| Document 全体の保存 | ✅ | manifest.json + document/main.ron（Composition・レイヤー・ネットワーク（subnet 入れ子含む）・キーフレーム・予約フィールド・media_assets、決定的 RON。メディアは相対 / 変数パスで記録、公開パラメータ宣言 `exposed_parameters` を含む、format v8）+ settings.toml。保存時に前リビジョンを `.bak` 化。v4 以前のファイルはロード時にベクタパラメータを畳み、v5 以前はカーブパラメータを変換する。v6 以前は宣言ゼロとして読む。v7 以前は作者が指定した色をリニアへ読み替える。宣言の追加・改名・並べ替え・削除は Properties の公開パラメータセクションから行える（EXPO-5） |
| 設定の適用（3 層マージ、`user` 層は未実装） | 🟡 | 起動時に `default → global → project` を解決して `AppSettings` Global に載せ、**`locale`、`[appearance]`（テーマモード / ライト・ダークのテーマ）、`playback.frame_rate` を適用**。言語と外観は環境設定ダイアログから、既定フレームレートはプロジェクト設定ダイアログから**変更でき、その場で反映される**（言語切替は開いている全ウィンドウを再描画し、メニューバーも組み直す。テーマ名が無効なときは同梱テーマへフォールバック）。未知のロケールは警告して `en` にフォールバック。既定フレームレートは新規コンポジションの初期値と `File ▸ New` の root コンプに効く（**アクティブなコンポジションがあればその書式が勝つ**ので、開いている状態では観測できない。fps 表記 / 有理数の両方を読み、解釈できない値は警告して 30 fps へフォールバック）。書き込み API は層ごとに独立（global = `<config>/ravel/settings.toml` へ即時アトミック、project = 次のプロジェクト保存で `.ravprj` に入り dirty になる）。失敗は通知。「既定に戻す」はその層の値を消す（既定値を書き戻さない）。`playback.stop_returns_to_play_start`（既定 `false`）と `startup.create_composition`（既定 `true`）も**適用済み**で、それぞれ停止の着地点と、起動時 / `File ▸ New` のドキュメントがコンポジションを 1 つ持つかを決める（既定はどちらも従来の挙動）。**環境設定 ▸ 一般**の switch 2 つで切り替えられ、`global` 層へ即時に書かれる。`[cache]` も**適用済み**（`SET-8`）で、**環境設定 ▸ キャッシュ**の 4 項目 — VRAM 上限 / RAM 上限 / sim 予約率 / 置き場 — がすべて `global` 層へ書かれる。上限と予約率は走行中の `SharedCacheBudget` へ即時に流れ（下げた分はその層で次に予約が起きたときに退避として回収される）、置き場はキャッシュを作る側が読むので**次回起動から**効く（既に書かれたファイルは移動しない）。**ディスク層の有無と上限は出していない** — `Tier::Disk` に課金する経路が無く、層の実装は `CACHE-11`。**自動保存（`SET-9`）・プロキシ（`SET-10`）・カラー管理（`SET-11`）への配線は未**で、前提機能が入るまで設定画面にも出さない。`user` 層は置き場も呼び出し元も無い |
| キーバインドのユーザー上書き | 🟡 | 起動時に `<config>/ravel/keybindings.toml` を既定アセットへ重ねる。同じコマンドを別 chord に割り当てると既定の chord は外れ、chord が既定と衝突すればユーザーが勝つ。ファイルが無いのは通常の初回起動、TOML として壊れていれば警告して既定のみ、解釈できない行はその行だけ警告して捨てる。バインドは `AppShell` 経由で登録されるので、ユーザー由来も同じ文脈述語付き（`!Input && !PopupMenu && !AppMenuBar` — テキスト入力に譲る `MED-APP-16` と、開いたメニューに譲る `MED-APP-31`）。環境設定 ▸ キーバインドに**読み取り専用の一覧**（全コマンド / 現在の chord / 由来 = 既定・ユーザー設定・パネル固有・割り当てなし）。パネル固有のバインドは `workspace.rs` の `PANEL_BINDINGS` という 1 つの表から一覧にも出るので、`P`（ペン、Viewer 限定）のようなものが「割り当てなし」に見えることはなく、どのパネル限定かも表示する。その表のコマンドは**ユーザーファイルから再割り当てできない**（受理するとコンテキストの無いグローバルバインドになるため、警告して捨てる）。**画面からの編集は未**（`SET-12`） |
| マイグレーション | ✅ | v1→v2→…→v8 連鎖（`manifest.json` が起点）。v4 はメディアアセットを相対 / 変数パスで持ち（v3 の絶対 `PathBuf` はそのまま `Absolute` として読める）、`assets/refs.json` を廃止。v2 以前（graph/main.ron のみ）は平坦 Graph を Document に包み、manifest の解像度/fps で root comp を生成。**v5 以降は manifest の版印だけを進め、ドキュメント本体の変換はロード後の型付きパスで行う**: v5 がベクタパラメータの畳み込み（`fold_component_params`）、v6 がカーブパラメータの変換（`upgrade_curve_params`）。v7 は公開パラメータ宣言の追加のみで、変換すべき既存の表現が無いため型付きパスを持たない（`#[serde(default)]` で宣言ゼロとして読む）。v8 はパイプラインがリニアになったことで作者指定の色の**意味**が変わったので型付きパスを持つ（`linearize_colors`）— ノードの `COLOR` パラメータ・コンプ背景色・公開パラメータの `color` 既定値を一度だけ `srgb → linear` に読み替え、変換できない箇所（式で駆動される色、キーフレーム間の補間のずれ）は警告として出す。**この変換は冪等ではないので、一度だけにするのは版印の仕事**。`Layer.audio` は既存 v4 への追加フィールド（欠落時 `None`）で版を上げていない。`Composition.guides`（ユーザーガイド、`SNAP-2`）も同じ扱いで、v8 への追加フィールド（欠落時は空）として入り版もマイグレーションも増やしていない |
| サブグラフテンプレート (`*.ravtpl`) | 🟡 | 形式と読み書き API は入っている（`ravel-project::subgraph_template`。RON、`<config>/ravel/subgraph-templates/`、アトミック書き込み、読めないファイルは飛ばす）。サブネットの内部グラフ + そのサブネット内に束縛された公開パラメータ宣言を持ち、貼り付けは ID を振り直して宣言の束縛も追従させる（`ravel-core::subgraph_template`）。**保存・読み込みの UI は無く、アプリからは呼ばれていない**（EXPO-6 はヘッドレス。UI は REQ-PLUGIN-005 として別計画） |
| ID カウンタ前進 | ✅ | ロード時に NodeId/EdgeId/CompId/LayerId カウンタをドキュメント最大 ID 超へ（REQ-LAYER-009） |
| undo 履歴 | ✅ | ロード/New は DocumentStore ごと差し替え（undo ステップにしない） |
| ジャーナル版管理 | ✅ | bincode ジャーナルにヘッダ（magic + version）。旧形式・版不一致は破棄（クラッシュジャーナルは揮発性の方針） |
| 未保存変更ガード | ✅ | 保存完了リビジョンで dirty 判定。New/Open/Quit/メインウィンドウ Close は Save / Discard / Cancel を確認し、Save 成功後だけ続行（保存中の再編集・失敗時は維持） |
| 自動保存・ジャーナルリプレイ復元 | 🔲 | REQ-PROJ-002、別計画 |
| コンポジション管理 | ✅ | 表示対象は `ActiveComposition` Global に一元化済み（レイヤー選択は `LayerSelection` Global、不変条件 `LayerSelection.comp == ActiveComposition`）。`Document.root_comp` は「開いたとき最初に active になるコンプ」で UI 切替では書き換えない。アクティブコンプは `ui_state.json` に永続化（欠落時 `root_comp` フォールバック。この UI 状態追加時は format_version 3 を据え置き、現行は v7）。作成・切替・複写・削除・設定編集は Composition メニュー / Cmd+K / Outliner から可能。設計 = REQ-UI-013 / `docs/implementation/done/outliner-comp-management-plan.md`（単位 1〜6 完了） |

---

## 書き出し（File ▸ Export…）

**ステータス**: `render-export-plan.md` 単位 5 完了。CLI からの同じ書き出しは
[`dev/render-cli.md`](dev/render-cli.md)、設計意図は
[`specifications/ui/render-queue.md`](specifications/ui/render-queue.md)。

| 項目 | 状態 | 備考 |
|------|------|------|
| 書き出しダイアログ | ✅ | `File ▸ Export…`（`CommandId::FileExport`）。対象コンプ（音声の有無は選び直しに追随）・フレーム範囲（画面上は両端 inclusive）・形式・PNG ビット深度・出力ディレクトリ・接頭辞 / 接尾辞 / ゼロ詰め桁数・上書き・音声の有無。OK を押すまでキューに何も届かない。空コンプと `out < in` はその場で拒否 |
| 形式一覧 | ✅ | 実行時列挙（`available_encoders`）から作る。CLI の `list codecs` / `--format` と同じ 1 経路。**使えない行も理由付きで出す** |
| 動画コンテナ | 🔲 | 一覧には出るが選べない。書き手が無いため（CLI 側の `codec-no-writer` と同じ拒否）。`render-export-plan.md` の非対象 |
| 連番書き出し | ✅ | PNG / EXR。ファイル名は絶対フレーム番号。CLI と同じワーカー・同じエンコーダを通ることを `ravel-app/tests/export_pipeline.rs` がバイト比較で確認する |
| 音声の併置 | ✅ | 音声を持つコンプは同じ範囲の WAV（48kHz ステレオ 32bit float）がフレームの横に出る。素材のデコードは `ffmpeg` フィーチャ依存 |
| 上書き拒否 | ✅ | 既定は拒否。ファイル名単位で判定し、**1 フレームも評価せずに**失敗する |
| ジョブの一時停止 / 再開・優先度変更 | 🔲 | REQ-RENDER-001 の残項目。あるのは中止のみ。引き受ける計画は未定 |

**実機確認済み**（2026-08-08）: `File ▸ Export…` から 1080p のコンポジションを
100 フレーム書き出し、**PNG 連番 100 枚と WAV が並んで出ること**を実物で確認した。
レンダーキューパネルの進捗表示も動く。

`ravel-cli render --param` で公開パラメータを差し替える往復も実機確認済み
（2026-08-07）: GUI で作った宣言を `ravel-cli list params` が読み、`--param` で
差し替えた出力が既定とバイト単位で違うことを確認した。

---

## ワークスペース・ドッキング・ウィンドウ

`crates/ravel-dock/`（描画）、`crates/ravel-app/src/{window_host,title_bar,
layout_persist,workspace_layouts}.rs`（配線）、`crates/ravel-ui/src/{layout,
layout_doc,preset,shell}.rs`（モデル）

**ステータス**: `done/free-pane-docking-plan.md`（DOCK-1〜10）完了。
gpui-component の `DockArea` 依存は撤去済み（`gpui_component::dock` への参照は
ワークスペースに 1 つも無い）。設計意図は
[`specifications/ui/workspaces.md`](specifications/ui/workspaces.md)。

| 項目 | 状態 | 備考 |
|------|------|------|
| N ウィンドウ × レイアウトツリー | ✅ | 全ウィンドウが同じホスト（`WindowHost`）。メインは `windows[0]`。論理 `WindowId` ↔ GPUI ハンドルは `WindowRegistry`（Global）1 箇所 |
| 同一パネルの多重インスタンス | ✅ | 全 16 種。タブ 1 枚 = `PanelInstance`。ビューは `PanelInstanceId` キーのレジストリ（`PanelViews`）がキャッシュ |
| 4 プリセットの表示と切替 | ✅ | `Cmd+F1`〜`F4` と Workspace メニュー。メインウィンドウのツリーだけを差し替え、分離ウィンドウは触らない |
| View トグル（既定スロット挿入） | ✅ | ツリーに無いパネルは `PanelKind::default_slot()` へ挿入。アクティブプリセットに依存しない |
| タブ切替・タブ D&D | ✅ | エリア端 1/4 = 分割、中央 = 合流、タブバーの帯 = 合流。ドロップ先はアクセント色でハイライト、結果が変わらないドロップは無効（カーソルが `OperationNotAllowed`）。4px でクリックとドラッグを分ける。Escape とボタン喪失でキャンセル |
| タブのウィンドウ間ドラッグ | ✅ | ウィンドウ外で離すと、カーソル下のワークスペースウィンドウへ移動（そのウィンドウの最初のエリアに合流）、無ければ新しい分離ウィンドウ。重なりは論理 ID の大きい方を優先（GPUI は重ね順を公開していない） |
| エリアメニュー（⋮） | ✅ | Split Right / Split Down / Duplicate into Split / Close Area。前 2 つはタブ 1 枚のエリアでは無効化 |
| スプリッタドラッグ | ✅ | 当たり幅 5px / 線 1px、比は `[0.05, 0.95]`。ドラッグ中はプレビュー、離したときに 1 回だけモデルへ書き戻す |
| 空エリアの畳み込み | ✅ | 空になったエリアは消え親 `Split` が畳まれる。最後のエリアが消えた分離ウィンドウは閉じる。メインの最後のタブは動かせない |
| detach / reattach | ✅ | View メニュー ▸ パネルを切り離す、または `Cmd+Shift+D`（フォーカス中インスタンス → 新ウィンドウ。**Timeline に focus があるあいだ chord はレイヤー分割に覆われる**ので、そのときはメニューから）/ `Cmd+Shift+R`（フォーカス窓の全パネル → メイン、ID 保持で既定スロットへ）。**`Cmd+Shift+D` の直後の `Cmd+Shift+R` は無効**（開いた窓の中のパネルがフォーカスを取っていないため。1 度クリックすれば動く） |
| 分離ウィンドウのクローズ | ✅ | クローズボタン = インスタンス破棄（メインへ自動で戻らない）。必ず `AppShell::close_window` を通るのでハンドル表と食い違わない（`MED-APP-01`） |
| 共通 TitleBar | ✅ | `RavelTitleBar` が全ウィンドウ。中央ラベル（メイン = プロジェクト名、分離 = パネル名 / 「N 個のパネル」）+ 窓種別スロット。中央寄せ補正はこの 1 箇所。**非 macOS は実機未検証** |
| アプリメニュー | ✅ | 出所は headless の `MenuBar` 1 つ。`workspace::install_menus` が唯一の出口で、macOS の OS メニューバー（`cx.set_menus`）と、**非 macOS でメインウィンドウのタイトルバーに出る `gpui_component::menu::AppMenuBar`**（`GlobalState` のスナップショット + `reload`）へ同じ `build_menus` を配る（`App::set_menus` を実装しているのは macOS だけ）。プラットフォーム分岐は `title_bar::render_main_title_bar` の `cfg!` 1 箇所。合成アプリメニュー（About / Services / 終了）はスナップショット側で落とす（終了はファイル、About はヘルプに既にある）。分離ウィンドウには出さない。**Windows / Linux は実機未検証** |
| 自前バーのキーボード操作 | 🔲 | 開閉とホバーでのトップレベル切替、メニュー内の上下移動は動く。**左右キーでのトップレベル移動は不可**（`assets/keybindings/default.toml` の `Left` / `Right` = 1 フレーム送りが `PopupMenu` コンテキストより後に登録され勝つ。ポップアップが開いている間の全ワークスペースショートカットに共通の既存問題）。**Escape で閉じない**のはパネルの `…` メニューなど既存の `PopupMenu` でも同じで、このバー固有ではない。閉じるにはクリック外しか項目選択 |
| AlwaysOnTop | ✅ | 分離ウィンドウのピンで実行時トグル、ウィンドウごとに独立。`WindowLayout.always_on_top` が正で、開くときにも適用（再起動後も固定のまま） |
| メイン窓連動 | ✅ | クローズ追従、最小化追従（復帰後はメインがキーウィンドウ）。フォーカスは連動しない |
| 分離ウィンドウのダイアログ / 通知 | ✅ | 全ウィンドウが `Root::render_dialog_layer` / `render_notification_layer` を置く |
| レイアウトの永続化 | ✅ | `<config>/ravel/layout.toml` に全ウィンドウのツリー・配置・AlwaysOnTop・名前付きレイアウトを保存。書き出しはコマンド 1 つごと（内容が変わったときだけ、バックグラウンド）と終了時。読めなければ既定レイアウトに倒す（`LOW-APP-14`） |
| 名前付きレイアウト | ✅ | Workspace ▸ Manage Layouts… で保存 / 適用 / 削除（`PresetLibrary::save_custom` への導線）。保存対象はメインウィンドウのツリーのみ |
| `.ravprj` 埋込 | ✅ | オプトイン（既定 OFF、トグルは Manage Layouts ダイアログ）。任意エントリ `workspace_layout.toml`。適用はセッション限定でアプリ既定を汚さない。埋込側にユーザーのプリセット集と埋込設定は書かない |
| ビュー状態のウィンドウ間移送 | 🔲 | detach 先は既定状態から始まり、分離窓内で作った状態は reattach で失われる（元の窓側の状態は保たれる）。GPUI のパネルフォーカス購読が窓に束縛されているため |
| ドラッグ中のタブのプレビュー | 🔲 | 運んでいるタブ自体は描かない（カーソルとドロップ先ハイライトのみ）。ウィンドウをまたぐドラッグでは落とすまで行き先が見えない |
| 新しい分離ウィンドウの位置 | 🔲 | 落とした位置ではなく画面中央 640×480 |
| Workspace メニューのプリセットチェック | 🔲 | チェックマーク自体は macOS の OS メニューにも自前バーにも出る（`convert_menu_item` が `MenuItem::checked` を立てる）。残る問題は**どれを指すか**で、手で組み替えても直前のビルトインを指したまま |
| ビューア専用全画面ウィンドウ / OCIO | 🔲 | REQ-UI-009 の残項目。全ウィンドウ同型モデルの上に載る後続機能 |

---

## その他パネル

| パネル | 状態 | 備考 |
|--------|------|------|
| MediaBin | ✅ | プロジェクトのメディアアセット一覧（media-import 計画 単位 4）。種別フィルタ（全て / 映像 / 静止画 / 音声）と名前検索、サムネイル（単位 5 の `ThumbnailCache`、生成前・失敗時は種別アイコン）、オフライン表示。選択は `MediaSelection` Global で Properties が `PropertiesTarget::MediaAsset` に追従（表示はプレースホルダ、作り込みは単位 6）。行の操作: ダブルクリック / 右クリックで「レイヤーとして追加」（再生ヘッド位置）、Timeline / Viewer へのドラッグでレイヤー化（Timeline はポインタ位置のフレーム、Viewer は再生ヘッド。複数素材でも 1 undo）「素材からコンポジションを作成」（素材の解像度・fps・長さ）「プロジェクトから削除」（使用中なら参照コンプ・レイヤー名つきで確認）。Relink… は単位 6 |
| Outliner | ✅ | Composition → Layer → Node の3階層ツリー、選択連動、active 切替、Unused グループ（単位 3）+ コンプの作成・複写・削除・設定（単位 4、Composition メニュー / ヘッダーボタン / 行の右クリック）。レイヤー操作（単位 5、D&D 並べ替え / 右クリックの Rename・Duplicate・Delete。ドラッグ中は `ResizeUpDown`）。複数選択（単位 6、Shift 範囲 / Cmd トグル、Duplicate・Delete は選択全体に 1 undo）。アクティブコンプ行の右クリックに「レイヤーを追加」サブメニュー（組み込みテンプレート 5 種。`LayerAdd*` Action を dispatch するので Layer メニューと同一経路）。検索・フィルタ欄と親子付け替え D&D は非対象 |
| Render Queue | ✅ | 書き出しジョブ一覧（`render-export-plan.md` 単位 5）。行は投入時点で出て「待機中」から始まり、ワーカーのイベントで 進捗バー・n / m フレーム・状態語が動く。中止ボタンは待機中・実行中の行だけに出し、フレーム境界で止めて書きかけの出力を消す。失敗行は `RenderError` の診断を下に出す。「完了分を消す」は終わった行だけを畳む。**キューはパネルではなくセッションが持つ**ので、パネルを閉じてもレンダリングは続き、開き直すと走っているジョブが見える |
| 属性スプレッドシート | ✅ | 選択ノードが評価したジオメトリの属性表（`attribute-spreadsheet-plan.md` 単位 3）。ドメインタブ（ポイント / プリミティブ / インスタンス / ディテール）に要素数が出て、0 要素のタブは押せない。列は要素番号（左端固定）→ 標準属性 → 名前順の残り、値は型別書式（F32 は有効数字 4 桁、非有限値は `NaN` / `inf` / `-inf` をそのまま表示）。行は `DataTable` の仮想スクロールなので上限を切っていない（実測: `scatter.grid` 1 万インスタンスを端から端までスクロールして CPU 約 40%／1 コア、コマ落ちなし）。表示対象は Properties と同じ `SelectedPropertiesTarget` の先頭ノードで、結果は Viewer の評価要求に相乗りしたスコープターゲット（`EvalResults`、キーは `(path, node)`）から読む。**read-only**。セル編集・統計表示・複数選択の同時表示・`instance_source` の中身は非対象。ソートと列並べ替えは未実装なので UI も出していない |
| Dopesheet | 🔲 | PlaceholderPanel |
| Histogram | 🔲 | PlaceholderPanel |
