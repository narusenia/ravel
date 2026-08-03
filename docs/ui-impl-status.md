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
| エッジスタイル描画 | ✅ | Bezier(S字), Straight(直線), Step(直角折れ線) + 各ヒットテスト |
| Copy/Paste (Cmd+C/V) | ✅ | ノード群+内部エッジをコピー、新IDでペースト |
| Duplicate (Cmd+D) | ✅ | 即時複製 (20,20) オフセット |
| ポート型フィルタリング | ✅ | 接続ドラッグ中に非互換ポートをスナップスキップ |
| 単一入力制約 | ✅ | 既存エッジを自動置換 |
| Fit View (F key) | ✅ | 全ノードが画面に収まるようズーム+パン |
| Evaluator 連携 | ✅ | ProjectState の EvalService 経由（Document-aware、バックグラウンド） |
| ネットワークコンテキスト | ✅ | 所有パス（Comp/Layer/[Subnet...]）で 1 ネットワークを編集（REQ-LAYER-011）。`LayerSelection` を observe し、**レイヤー 1 つだけ選択中**のときそのネットワークを開く。0 個と複数個は同じ閉じた状態（中央メッセージのみ差し替え、閉じるとき `CanvasSelection` もクリア。REQ-UI-013 単位 6） |
| サブネットへの潜り | ✅ | サブネットノードをダブルクリックで内部 Graph へ |
| Subnet ノードの追加 | ✅ | Add Node / パレットから作った Subnet が内部 `net.in` / `net.out` ペアを持ち、そのまま評価でき（既定の出力ピンは `frame`）ダブルクリックで潜れる。外側ピンは内部 In / Out から導出され、内部のポートを追加・削除・改名・型変更・並び替えすると同じ Document コミットで外側ピンと外側エッジが追随する（**1 操作 1 undo**）。名前で対応付けるので並び替えで配線は保たれ、消えたピンのエッジだけが落ちる。ドリフトしたピンはロード時に修復される（内部グラフを持たない旧データは対象外）。ノード群をまとめる Collapse / Extract は未実装（network-interface-editing 計画 単位 6） |
| パンくずバー | ✅ | Comp / Layer / Subnet... を表示、クリックで任意の深さへ戻る |
| synthetic ノード非表示 | ✅ | `NodeMetadata.synthetic` を描画・ヒットテスト両方でフィルタ |
| ノード処理時間表示 | ✅ | ノード下に評価時間（例 12ms）。8ms 以上で黄、33ms 以上で赤 |
| ポインタフィードバック | ✅ | ポート / 空白=`Crosshair`、ノード=`OpenHand`、エッジ=`PointingHand`。接続スナップ時は `DragLink`、移動 / パン中は `ClosedHand` |
| ノード検索パレット | ✅ | Tab（トグル、キャンバス中央）/ 空所ダブルクリック（カーソル位置）/ ワイヤーを空所にドロップ（接続可能な型のみ候補）。ロケール解決後の label + description を大小無視の部分一致で検索し、label 一致が description 一致より上位、その同じ段の中で最近使用（セッション内メモリ、最大 10 件、永続化なし）が上位。カテゴリフィルタチップ、候補行にアイコン + ラベル + カテゴリ。↑↓/Enter/Escape は入力の capture フェーズで処理。閉じると状態は残らない。確定は右クリックメニューと同じ経路（1 undo）。**↑↓/Enter/Escape の実イベントテストは未**（絞り込み・発動・配線はテスト済み） |
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
| Vector フィールド | ✅ | `Channel2` / `Channel3` パラメータを成分ごとの ScrubInput の横並び 1 行で表示・編集。組み込みノードのベクタパラメータ（`shape.*` の `center`、`shape.ellipse` の `radius`、`scatter.grid` の `spacing`、`geometry.transform` の `translate` / `rotation` / `scale` / `pivot`、`transform` の `translate`、`field.falloff` の `center` / `direction`、`scatter.scatter` の `area`、`type` が `vec2` / `vec3` の `attribute.set` の `value`）が到達する。成分ラベルとリンクトグルは未実装（MED-APP-20）。4 成分（`attribute.set` の `type = "vec4"`）は Color 描画のまま（MED-APP-19） |
| Curve フィールド | ✅ | `ParameterValue::Curve`（`field.curve_remap` の `points`）が到達。折り畳み時はカーブのサムネイル、行クリックで直下にインラインエディタを展開。複数行を同時に展開でき、展開高さはハンドルドラッグで変更。展開部はグリッド + 軸目盛（表示範囲から導出、短い軸ではラベルを間引く。表示範囲は f64 で保持し深いズームでも潰れない）、選択点の入力/出力の数値表示・編集、補間種別（Linear/Bezier/Step）の切替、ベジエ接線ドラッグ（Shift で 45 度スナップ）、表示範囲 min/max の数値編集、ホイールズーム、Fit（ベジエ接線ハンドルも可視域に含める）を持つ。展開状態・高さ・表示範囲・選択はビュー状態で Document に入らない（undo 対象外、ターゲット切替で展開はリセット） |
| Enum フィールド | ✅ | ラベル + 値表示 + Select ドロップダウン |
| Bool/String/Color | ✅ | key-value テキスト表示 (将来: 専用ウィジェット) |
| Ports セクション | ✅ | `net.in` / `net.out` 選択時のみ表示（network-interface-editing 計画 単位 3）。ノードが宣言する全ポートを 1 行 1 ポートで列挙し、**固定ポート（`net.in` の `base_geometry` / `t` / `f` / `source`、`net.out` の `frame`）は読み取り専用行**（名前と型のみ、ツールチップで組み込みと明示）。カスタム行は名前 Input・型 Select・上下移動・削除ボタンを持ち、末尾に追加行（名前 + 型 + `+`）。型 Select の選択肢は文脈依存（レイヤールートの In は値型 6 種、サブネット内 In は全 10 種、Out は 8 種 — `Int` / `Bool` は Out 側に種別の置き場が無く `Float` と区別できないので提示しない）。拒否された編集（重複名・予約名・許可されない型・空名）はセクション下に理由を表示 |
| 空状態プレースホルダー | ✅ | ノード未選択時に表示 |

### インタラクション

| 操作 | 状態 | 詳細 |
|------|------|------|
| ノード選択連動 | ✅ | SelectedPropertiesTarget Global で自動切替 |
| レイヤー選択連動 | ✅ | Timeline / Outliner のレイヤー選択で Layer セクション表示・編集（殻属性: 時間配置/Transform/opacity/blend/adjustment、音声を持つレイヤーでは Audio セクションの gain/fade/audio mute、およびアセットが持つ音声ストリーム一覧からの選択（コンテナのストリーム番号 + codec/rate/ch。一覧は `AssetMetadata` 由来で probe しない）、ProjectState 経由で Document 更新）。複数選択時は読み取り専用の Layers ターゲット（選択数 + 共通値、相違は「—」。一括編集は後半） |
| In カスタムパラメータ | ✅ | `custom.<name>` フィールドとして表示・編集（REQ-LAYER-002）。編集は In ノードのパラメータへ書き戻し |
| Bool 編集（レイヤー） | ✅ | solo/muted/locked/adjustment を Checkbox で編集 |
| スクラブでパラメータ変更 | ✅ | 感度=UI レンジ由来、clamp=hard レンジ。Shift=10x / Cmd=0.1x。NodeEditorHandle 経由の deferred direct call で Graph 更新 |
| クリックでテキスト入力 | ✅ | gpui-component Input（EntityInputHandler 経由）。全選択で開始、Enter/blur で確定・clamp、パース不能は復元。IME 実機確認は未 (#41) |
| Select でパラメータ変更 | ✅ | Enum パラメータ (merge operation、`attribute.set` の `type` 等)。`type` の変更は `value` のアリティも変え、露出済みパラメータポートの型を追随させる（合わなくなったエッジは破棄。値・ポート・エッジで 1 undo） |
| カスタムポートの編集 | ✅ | Ports セクションからの追加・改名・型変更・並び替え・削除。いずれも `NodeEditorHandle` 経由の deferred direct call → `commit_graph` で **1 操作 1 undo**（ポート・同名パラメータ・巻き添えのエッジが 1 スナップショット）。型変更はポートの index を保つ（新しい型を運べないエッジのみ破棄、パラメータは新しい型の既定値に置き換わる）。並び替えは固定ポートを跨がない。改名と削除はノードエディタのポート右クリックからも同じ経路で行える（単位 4、NodeEditor の表を参照） |
| undo/redo | ✅ | Document 単位 undo（ProjectState）。**undo 単位=ジェスチャ**（スクラブ中の Change は undo を積まず、ドラッグ終了の Commit で 1 スナップショット） |
| キーフレームトグル (◆/◇) | ✅ | アニメート可能フィールド左のダイヤボタンで現在フレームにキー追加/削除（1 undo）。殻 Transform/Opacity/Audio Gain・custom.*・ノード Float/Channel* 対象。定数 Float は Channel 化（REQ-LAYER-004） |
| アニメーションチャネル保持 | ✅ | キーフレーム付きチャネルのスクラブは平坦化せず現在フレームにキー挿入/更新（殻・custom.*・ノードパラメータ共通） |
| カーブ点の編集 | ✅ | インライン展開したカーブエディタで点をドラッグ移動、空所ダブルクリックで追加、点のダブルクリックで削除、クリックで選択。**両端 2 点は x 固定（y のみ編集可）** — 両端はカーブの定義域そのもの。定義域が変わるのは明示的な 2 操作だけ（定義域の外側への点の追加で広がる / 端の削除で縮む。ただし 2 点のときは削除不可）。選択点は数値でも編集可（非有限値は拒否して直前値に戻す）。**undo 単位=ジェスチャ**（ドラッグ中の Change は積まず、終了の Commit で 1 スナップショット。接線ドラッグ・数値編集も同じ）。展開・折り畳み・ズーム・Fit は値に影響せず undo にも積まない |
| 値ラベルリアルタイム更新 | ✅ | スクラブ中に値表示更新 |

### ファイル構成

| ファイル | 役割 |
|---------|------|
| `ravel-ui/src/properties/mod.rs` | PropertySection, PropertyField, PropertyValue 型定義 |
| `ravel-ui/src/properties/node.rs` | ノード用セクション生成 (NodeInfo, Parameters, Ports) |
| `ravel-ui/src/properties/layer.rs` | レイヤー用セクション生成 (Layer, Transform, Timing, Compositing) |
| `ravel-app/src/panels/properties.rs` | PropertiesGpuiPanel (GPUI描画、ウィジェット管理) |
| `ravel-app/src/widgets/scrub_input.rs` | ScrubInput（スクラブ + テキスト編集の数値ウィジェット） |
| `ravel-app/src/widgets/param_curve_editor.rs` | ParamCurveEditor（`CurveParam` のインラインエディタ。座標変換と接線スナップは `widgets/curve_editor.rs` と共有） |
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
| プロパティ展開行 | ✅ | 殻の Position/Scale/Rotation/Opacity + キーフレームを持つネットワーク内パラメータ（In カスタム・サブネット露出含む、REQ-LAYER-004） |
| キーフレームダイヤ | ✅ | Keyframes チャンネルをレイヤーローカル→Comp 時間へ変換して描画（`comp_frame_for_key`、in_frame 考慮）。選択中は描き分け |
| 再生ヘッド | ✅ | 赤色 2px 縦線 |
| タイムコード表示 | ✅ | ヘッダー左上コーナーに M:SS:FF（再生ヘッド位置、固定幅表示） |
| 選択ハイライト | ✅ | レイヤーヘッダー背景色変更 |

### インタラクション

| 操作 | 状態 | 詳細 |
|------|------|------|
| 再生ヘッド移動 (ルーラークリック) | ✅ | クリック位置のフレームに移動 + PlaybackClock を seek |
| 再生ヘッドスクラブ (ルーラードラッグ) | ✅ | ドラッグで連続追従 + PlaybackClock を seek（再生位置・評価フレームに反映） |
| 水平スクロール | ✅ | マウスホイール dx、scroll_offset 更新 |
| 垂直スクロール | ✅ | レイヤーリスト領域 overflow_y_scroll |
| ズーム (Cmd/Ctrl+スクロール) | ✅ | カーソル位置アンカー、pixels_per_frame [0.1, 50.0] |
| レイヤー選択 (ヘッダー/バークリック) | ✅ | `LayerSelection` Global へ書き込み → Properties / ノードエディタが observe。Shift で範囲選択、Cmd（platform 修飾）でトグル（REQ-UI-013 単位 6、修飾クリックはバー移動・並べ替えを開始しない）。選択中の全レイヤーをハイライト。削除・複写・S/M/L は選択全体に効く（各 1 undo、ロック済みは削除から保護。S/M/L は行本体の選択を奪わない） |
| ネットワークを開く | ✅ | レイヤーを 1 つ選択するとノードエディタが `LayerSelection` を observe して開く。ダブルクリック（ヘッダー/バー）は加えてビューを fit する。0 個・複数個選択時は閉じた状態 |
| レイヤー展開 (▶/▼) | ✅ | プロパティグループ・チャンネル行の開閉 |
| Solo/Mute/Lock トグル | ✅ | Document 更新（solo/mute は Structural 再評価） |
| レイヤー作成 | ✅ | Layer メニュー（Solid/Shape/Video/Null、テンプレートから生成） |
| レイヤー削除 | ✅ | Delete/Backspace（locked は保護）、Document undo で復元 |
| Document/undo 統合 | ✅ | 追加・削除・並べ替え・トリム・移動すべて Document 単位 undo |
| レイヤーバードラッグ移動 | ✅ | バー本体ドラッグ = start_frame 移動、端 6px = in/out トリム。1 ジェスチャ = 1 undo |
| レイヤー並べ替え | ✅ | ヘッダー縦ドラッグ |
| ポインタフィードバック | ✅ | ルーラー / トリム端、バー、ロック、キー / グラフアンカー / 接線を既存ヒット境界で区別。ドラッグ中も操作別カーソルを維持 |
| キーフレーム選択・移動 | ✅ | ダイヤクリックで選択+ドラッグ移動（live apply、mouse-up で 1 undo）。空所クリックで選択解除 |
| キーフレーム追加 | ✅ | チャンネル行の空所ダブルクリックでそのフレームに追加（現在値、1 undo） |
| キーフレーム削除 | ✅ | ダイヤ選択中の Delete/Backspace はキーフレームのみ削除（未選択時は従来通りレイヤー削除）。locked 保護あり |
| 再生/停止連携 | ✅ | PlaybackController（Space/K/←/→、メニュー）が playhead を駆動。follow トグル（コーナーの F）で表示範囲がページ追従 |

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
- タイムラインのプロパティツリーはレイヤーのトップレベルネットワークの
  パラメータのみ列挙する（サブネット露出パラメータはサブネットノードの
  パラメータとして現れる）。サブネット内部ノードのキーフレームは
  ノードエディタでサブネットに潜って編集する（ツリーへの再帰列挙は v2）。

---

## Viewer パネル

`crates/ravel-app/src/panels/viewer.rs`

| 項目 | 状態 | 備考 |
|------|------|------|
| FrameBuffer 表示 | ✅ | `ViewerFrame` Global 経由、`img` 要素 + `ObjectFit::ScaleDown`（アスペクト維持・拡大なし） |
| root comp 常時評価 | ✅ | ProjectState が Document 変更・再生位置ごとに root comp 出力（殻コンパイル + Document-aware 評価）を要求（REQ-LAYER-007）。選択ノードの単独プレビューは不採用（ユーザー判断で削除） |
| Geometry 自動ラスタライズ | ✅ | 評価ワーカーの `GpuEvalHooks::finalize` で CPU reference により rasterize（GPU texture Viewer は後続） |
| コンプ背景と透過確認 | ✅ | `Composition.background_color` は `comp.background` として評価結果へ合成。表示下地をコンプ背景 / 固定セルのチェッカーボード / 黒単色からセッション内で切替 |
| 未選択時プレースホルダ | ✅ | `viewer.no_output` locale キー |
| 再生・スクラブ・タイム同期 | ✅ | PlaybackController が再生/シーク毎に ProjectState へ root comp 評価を要求（latest-wins、ドロップ数カウント）。音声同期も実装済み: 音声トラックあり + デバイス稼働時は `SyncClock` が再生位置の正（`ClockSource::Audio`）、それ以外は従来の wall clock（`audio-plan.md` 単位 3） |
| GPU テクスチャ共有（ゼロコピー） | 🔲 | 現状は評価ワーカーで 1 回読み戻し → `RenderImage`（BGRA u8）変換して表示。GPUI-CE レンダラとの共有サーフェスは Phase 4 ストレッチ |
| ツールバー（選択/ペン等） | ✅ | 選択 / ペン / 矩形 / 楕円 / ハンド / ズーム（`ToolState` Global、REQ-UI-011、`tool-system-plan.md`） |
| 選択 bbox とハンドル | ⚠️ | **ハンドルは描画のみで動作を持たない** — スケール / 回転のジェスチャーはコード上に存在せず、動くのは bbox 内側からの移動だけ（担当は `viewer-overlay-manipulator-plan.md` の OVL-7）。ノード選択（`CanvasSelection`）はハンドル付き bbox。**レイヤー選択が 2 枚以上のときはレイヤー単位 bbox**（そのネットワークの shape ノードの bounds の和 → シェル変換、ハンドル無し。shape ノードを持たないレイヤーは出さない、REQ-UI-013 単位 6） |
| 複数レイヤーの同時ドラッグ | ✅ | レイヤー bbox の内側からドラッグで選択レイヤー全体を移動。`center` ベクタ再構築方式（REQ-UI-011）を全 target 分 1 つの Document に適用 → 1 undo。シェル変換が単位行列でないレイヤーは対象外 |
| ポインタフィードバック | ✅ | 描画、選択本体、パスアンカー / 接線、ペン閉路位置を区別。パン / 移動 / 描画中もカーソルを維持。未実装の Hand / Zoom と bbox リサイズには割り当てない |

評価はバックグラウンドワーカー（root comp は Composition 解像度）。
フレームは共有 `PlaybackPosition`（再生ヘッド位置）に従い、編集中も
一時停止中のフレームを再評価する。latest-wins でスクラブ Change・
再生フレームを間引き、UI スレッドは要求投函のみ。再生位置のクロックは
音声トラックあり + デバイス稼働時はオーディオデバイス（`SyncClock`）が
マスター、それ以外は wall clock にフォールバック
（`docs/implementation/audio-plan.md` 単位 3）。

---

## プロジェクト永続化（File メニュー）

**ステータス**: `.ravprj` フォーマット v4

| 項目 | 状態 | 備考 |
|------|------|------|
| New / Open / Save / Save As | ✅ | File メニュー配線済み。Save As/Open は GPUI ネイティブダイアログ。未保存時の Save は Save As にフォールスルー。dirty な New/Open は保存確認後に続行 |
| メディアインポート | ✅ | File ▸ Import…（`CommandId::FileImport`、Cmd+I、複数選択）と OS からのファイル D&D（REQ-UI-010）。probe は background executor、成功分だけ `media_assets` に相対化して登録し、再生ヘッド位置に素材長のレイヤーを作成。バッチ全体で 1 undo。同じ絶対パスは既存アセットを再利用。音声つきの素材は同じ 1 undo の中で殻に `AudioSource`（同一 asset_id + 最初の音声ストリーム）も設定し、映像を持たない音声ファイルは frameless な `audio` テンプレートでレイヤー化（`audio-plan.md` 単位 4） |
| UI 状態の保存 | ✅ | `ui_state.json`（アクティブコンプ）。任意エントリで、欠落時は `root_comp` フォールバック。既存 v3 アーカイブと互換（format_version 据え置き、REQ-UI-013） |
| ワークスペースレイアウトの埋込 | ✅ | 任意エントリ `workspace_layout.toml`。**オプトイン（既定 OFF）**で、OFF のときは書かれない（format_version 据え置き）。詳細は下の[ワークスペース節](#ワークスペースドッキングウィンドウ) |
| Document 全体の保存 | ✅ | manifest.json + document/main.ron（Composition・レイヤー・ネットワーク（subnet 入れ子含む）・キーフレーム・予約フィールド・media_assets、決定的 RON。メディアは相対 / 変数パスで記録、format v5）+ settings.toml。保存時に前リビジョンを `.bak` 化。v4 以前のファイルはロード時にベクタパラメータを畳む |
| 設定の適用（3 層マージ、`user` 層は未実装） | 🟡 | 起動時に `default → global → project` を解決して `AppSettings` Global に載せ、**`locale` を適用**（`settings.toml` に `locale = "ja"` があれば UI が日本語になる）。未知のロケールは警告して `en` にフォールバック。書き込み API は層ごとに独立（global = `<config>/ravel/settings.toml` へ即時アトミック、project = 次のプロジェクト保存で `.ravprj` に入り dirty になる）。失敗は通知。**設定 UI・テーマ・既定フレームレート・キャッシュ予算・自動保存への配線は未**（`settings-screen-plan.md` の SET-2〜8）。`user` 層は置き場も呼び出し元も無い |
| キーバインドのユーザー上書き | 🟡 | 起動時に `<config>/ravel/keybindings.toml` を既定アセットへ重ねる。同じコマンドを別 chord に割り当てると既定の chord は外れ、chord が既定と衝突すればユーザーが勝つ。ファイルが無いのは通常の初回起動、TOML として壊れていれば警告して既定のみ、解釈できない行はその行だけ警告して捨てる。バインドは `AppShell` 経由で登録されるので、ユーザー由来も `!Input` コンテキスト付き（`MED-APP-16`）。環境設定 ▸ キーバインドに**読み取り専用の一覧**（コマンド / 現在の chord / 由来 = 既定・ユーザー設定・割り当てなし）。**画面からの編集は未**（`SET-12`）。パネル固有のバインドはコード側にしか無く上書きの対象外 |
| マイグレーション | ✅ | v1→v2→v3→v4 連鎖。v4 はメディアアセットを相対 / 変数パスで持ち（v3 の絶対 `PathBuf` はそのまま `Absolute` として読める）、`assets/refs.json` を廃止。v2 以前（graph/main.ron のみ）は平坦 Graph を Document に包み、manifest の解像度/fps で root comp を生成。`Layer.audio` は既存 v4 への追加フィールド（欠落時 `None`）で、v5/migration は追加しない |
| ID カウンタ前進 | ✅ | ロード時に NodeId/EdgeId/CompId/LayerId カウンタをドキュメント最大 ID 超へ（REQ-LAYER-009） |
| undo 履歴 | ✅ | ロード/New は DocumentStore ごと差し替え（undo ステップにしない） |
| ジャーナル版管理 | ✅ | bincode ジャーナルにヘッダ（magic + version）。旧形式・版不一致は破棄（クラッシュジャーナルは揮発性の方針） |
| 未保存変更ガード | ✅ | 保存完了リビジョンで dirty 判定。New/Open/Quit/メインウィンドウ Close は Save / Discard / Cancel を確認し、Save 成功後だけ続行（保存中の再編集・失敗時は維持） |
| 自動保存・ジャーナルリプレイ復元 | 🔲 | REQ-PROJ-002、別計画 |
| コンポジション管理 | ✅ | 表示対象は `ActiveComposition` Global に一元化済み（レイヤー選択は `LayerSelection` Global、不変条件 `LayerSelection.comp == ActiveComposition`）。`Document.root_comp` は「開いたとき最初に active になるコンプ」で UI 切替では書き換えない。アクティブコンプは `ui_state.json` に永続化（欠落時 `root_comp` フォールバック。この UI 状態追加時は format_version 3 を据え置き、現行は v5）。作成・切替・複写・削除・設定編集は Composition メニュー / Cmd+K / Outliner から可能。設計 = REQ-UI-013 / `docs/implementation/done/outliner-comp-management-plan.md`（単位 1〜6 完了） |

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
| detach / reattach | ✅ | `Cmd+Shift+D`（フォーカス中インスタンス → 新ウィンドウ）/ `Cmd+Shift+R`（フォーカス窓の全パネル → メイン、ID 保持で既定スロットへ）。**`Cmd+Shift+D` の直後の `Cmd+Shift+R` は無効**（開いた窓の中のパネルがフォーカスを取っていないため。1 度クリックすれば動く） |
| 分離ウィンドウのクローズ | ✅ | クローズボタン = インスタンス破棄（メインへ自動で戻らない）。必ず `AppShell::close_window` を通るのでハンドル表と食い違わない（`MED-APP-01`） |
| 共通 TitleBar | ✅ | `RavelTitleBar` が全ウィンドウ。中央ラベル（メイン = プロジェクト名、分離 = パネル名 / 「N 個のパネル」）+ 窓種別スロット。中央寄せ補正はこの 1 箇所。**非 macOS は実機未検証** |
| AlwaysOnTop | ✅ | 分離ウィンドウのピンで実行時トグル、ウィンドウごとに独立。`WindowLayout.always_on_top` が正で、開くときにも適用（再起動後も固定のまま） |
| メイン窓連動 | ✅ | クローズ追従、最小化追従（復帰後はメインがキーウィンドウ）。フォーカスは連動しない |
| 分離ウィンドウのダイアログ / 通知 | ✅ | 全ウィンドウが `Root::render_dialog_layer` / `render_notification_layer` を置く |
| レイアウトの永続化 | ✅ | `<config>/ravel/layout.toml` に全ウィンドウのツリー・配置・AlwaysOnTop・名前付きレイアウトを保存。書き出しはコマンド 1 つごと（内容が変わったときだけ、バックグラウンド）と終了時。読めなければ既定レイアウトに倒す（`LOW-APP-14`） |
| 名前付きレイアウト | ✅ | Workspace ▸ Manage Layouts… で保存 / 適用 / 削除（`PresetLibrary::save_custom` への導線）。保存対象はメインウィンドウのツリーのみ |
| `.ravprj` 埋込 | ✅ | オプトイン（既定 OFF、トグルは Manage Layouts ダイアログ）。任意エントリ `workspace_layout.toml`。適用はセッション限定でアプリ既定を汚さない。埋込側にユーザーのプリセット集と埋込設定は書かない |
| ビュー状態のウィンドウ間移送 | 🔲 | detach 先は既定状態から始まり、分離窓内で作った状態は reattach で失われる（元の窓側の状態は保たれる）。GPUI のパネルフォーカス購読が窓に束縛されているため |
| ドラッグ中のタブのプレビュー | 🔲 | 運んでいるタブ自体は描かない（カーソルとドロップ先ハイライトのみ）。ウィンドウをまたぐドラッグでは落とすまで行き先が見えない |
| 新しい分離ウィンドウの位置 | 🔲 | 落とした位置ではなく画面中央 640×480 |
| Workspace メニューのプリセットチェック | 🔲 | 手で組み替えても直前のビルトインを指したまま。ネイティブメニューはチェックマークを描けないので現状は不可視 |
| ビューア専用全画面ウィンドウ / OCIO | 🔲 | REQ-UI-009 の残項目。全ウィンドウ同型モデルの上に載る後続機能 |

---

## その他パネル

| パネル | 状態 | 備考 |
|--------|------|------|
| MediaBin | ✅ | プロジェクトのメディアアセット一覧（media-import 計画 単位 4）。種別フィルタ（全て / 映像 / 静止画 / 音声）と名前検索、サムネイル（単位 5 の `ThumbnailCache`、生成前・失敗時は種別アイコン）、オフライン表示。選択は `MediaSelection` Global で Properties が `PropertiesTarget::MediaAsset` に追従（表示はプレースホルダ、作り込みは単位 6）。行の操作: ダブルクリック / 右クリックで「レイヤーとして追加」（単位 3 のインポート経路を再利用）「素材からコンポジションを作成」（素材の解像度・fps・長さ）「プロジェクトから削除」（使用中なら参照コンプ・レイヤー名つきで確認）。Relink… は単位 6 |
| Outliner | ✅ | Composition → Layer → Node の3階層ツリー、選択連動、active 切替、Unused グループ（単位 3）+ コンプの作成・複写・削除・設定（単位 4、Composition メニュー / ヘッダーボタン / 行の右クリック）。レイヤー操作（単位 5、D&D 並べ替え / 右クリックの Rename・Duplicate・Delete。ドラッグ中は `ResizeUpDown`）。複数選択（単位 6、Shift 範囲 / Cmd トグル、Duplicate・Delete は選択全体に 1 undo）。検索・フィルタ欄と親子付け替え D&D は非対象 |
| Dopesheet | 🔲 | PlaceholderPanel |
| Histogram | 🔲 | PlaceholderPanel |
