# UI 実装状況

各パネルの実装済み挙動・描画要素・未実装項目を記録する。

## Node Graph Editor (`panels/node_editor.rs`)

**ステータス**: TASK-014〜017 Done / layer-network Phase 3 Done
（ネットワークコンテキスト化）

### 描画要素

| 要素 | 状態 | 詳細 |
|------|------|------|
| グリッドドット背景 | ✅ | ズームに応じてスペーシング変動、spacing < 5px で非表示 |
| ノード矩形 | ✅ | 角丸 6px、テーマ背景色 + ボーダー、ヘッダーラベル |
| ポートドット | ✅ | 入力=左端、出力=右端、DataTypeId ごとの Hsla カラー |
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
| コンテキストメニュー (ノード追加) | ✅ | 右クリックで registry の全テンプレートから追加 |
| グリッドスナップ (ドラッグ中) | ✅ | 20px グリッドにスナップ |
| コンテキストメニュー (ノード削除) | ✅ | 右クリック → Delete Node |
| コンテキストメニュー (バイパス) | ✅ | 右クリック → Bypass Node (前後接続維持) |
| コンテキストメニュー (エッジスタイル切替) | ✅ | Edge Style → Bezier/Straight/Step |
| エッジスタイル描画 | ✅ | Bezier(S字), Straight(直線), Step(直角折れ線) + 各ヒットテスト |
| Copy/Paste (Cmd+C/V) | ✅ | ノード群+内部エッジをコピー、新IDでペースト |
| Duplicate (Cmd+D) | ✅ | 即時複製 (20,20) オフセット |
| ポート型フィルタリング | ✅ | 接続ドラッグ中に非互換ポートをスナップスキップ |
| 単一入力制約 | ✅ | 既存エッジを自動置換 |
| Fit View (F key) | ✅ | 全ノードが画面に収まるようズーム+パン |
| Evaluator 連携 | ✅ | ProjectState の EvalService 経由（Document-aware、バックグラウンド） |
| ネットワークコンテキスト | ✅ | 所有パス（Comp/Layer/[Subnet...]）で 1 ネットワークを編集（REQ-LAYER-011）。タイムラインのダブルクリックで開く。レイヤー選択では切替しない |
| サブネットへの潜り | ✅ | サブネットノードをダブルクリックで内部 Graph へ |
| パンくずバー | ✅ | Comp / Layer / Subnet... を表示、クリックで任意の深さへ戻る |
| synthetic ノード非表示 | ✅ | `NodeMetadata.synthetic` を描画・ヒットテスト両方でフィルタ |
| ノード処理時間表示 | ✅ | ノード下に評価時間（例 12ms）。8ms 以上で黄、33ms 以上で赤 |
| Document 単位 undo | ✅ | ネットワーク編集は Document へ splice（replace_network）→ ProjectState commit。undo/redo はパネルでは処理せずワークスペース → Document undo |
| ミニマップ | 🔲 | 後続タスク |

### ファイル構成

| ファイル | 役割 |
|---------|------|
| `ravel-app/src/node_editor/viewport.rs` | Viewport 座標変換、ズーム、fit_to_content |
| `ravel-app/src/node_editor/bezier.rs` | ベジェ曲線計算、距離ヒットテスト |
| `ravel-app/src/node_editor/painting.rs` | canvas 描画関数群、ポートヒットテスト、スナップ検出 |
| `ravel-app/src/node_editor/port_colors.rs` | DataTypeId → Hsla マッピング |
| `ravel-app/src/panels/node_editor.rs` | Panel 実装、DragMode 状態機械、イベントハンドラ |
| `ravel-core/src/registry/` | NodeRegistry + NodeTemplate + builtin 5種 |

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
| Float/Int フィールド | ✅ | ラベル + ScrubInput（ドラッグスクラブ + クリックでテキスト編集） |
| Enum フィールド | ✅ | ラベル + 値表示 + Select ドロップダウン |
| Bool/String/Color | ✅ | key-value テキスト表示 (将来: 専用ウィジェット) |
| 空状態プレースホルダー | ✅ | ノード未選択時に表示 |

### インタラクション

| 操作 | 状態 | 詳細 |
|------|------|------|
| ノード選択連動 | ✅ | SelectedPropertiesTarget Global で自動切替 |
| レイヤー選択連動 | ✅ | Timeline のレイヤー選択で Layer セクション表示・編集（殻属性: 時間配置/Transform/opacity/blend/adjustment、ProjectState 経由で Document 更新） |
| In カスタムパラメータ | ✅ | `custom.<name>` フィールドとして表示・編集（REQ-LAYER-002）。編集は In ノードのパラメータへ書き戻し |
| Bool 編集（レイヤー） | ✅ | solo/muted/locked/adjustment を Checkbox で編集 |
| スクラブでパラメータ変更 | ✅ | 感度=UI レンジ由来、clamp=hard レンジ。Shift=10x / Cmd=0.1x。PropertyChanged Global → NodeEditorPanel で Graph 更新 |
| クリックでテキスト入力 | ✅ | gpui-component Input（EntityInputHandler 経由）。全選択で開始、Enter/blur で確定・clamp、パース不能は復元。IME 実機確認は未 (#41) |
| Select でパラメータ変更 | ✅ | Enum パラメータ (merge operation 等) |
| undo/redo | ✅ | Document 単位 undo（ProjectState）。**undo 単位=ジェスチャ**（スクラブ中の Change は undo を積まず、ドラッグ終了の Commit で 1 スナップショット） |
| キーフレームトグル (◆/◇) | ✅ | アニメート可能フィールド左のダイヤボタンで現在フレームにキー追加/削除（1 undo）。殻 Transform/Opacity・custom.*・ノード Float/Channel* 対象。定数 Float は Channel 化（REQ-LAYER-004） |
| アニメーションチャネル保持 | ✅ | キーフレーム付きチャネルのスクラブは平坦化せず現在フレームにキー挿入/更新（殻・custom.*・ノードパラメータ共通） |
| 値ラベルリアルタイム更新 | ✅ | スクラブ中に値表示更新 |

### ファイル構成

| ファイル | 役割 |
|---------|------|
| `ravel-ui/src/properties/mod.rs` | PropertySection, PropertyField, PropertyValue 型定義 |
| `ravel-ui/src/properties/node.rs` | ノード用セクション生成 (NodeInfo, Parameters) |
| `ravel-ui/src/properties/layer.rs` | レイヤー用セクション生成 (Layer, Transform, Timing, Compositing) |
| `ravel-app/src/panels/properties.rs` | PropertiesGpuiPanel (GPUI描画、ウィジェット管理) |
| `ravel-app/src/widgets/scrub_input.rs` | ScrubInput（スクラブ + テキスト編集の数値ウィジェット） |
| `ravel-app/src/panels/mod.rs` | PropertiesTarget, PropertyChanged Global |

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
| レイヤー選択 (ヘッダー/バークリック) | ✅ | SelectedPropertiesTarget::Layer 発行 → Properties 連動。ノードエディタのコンテキストは奪わない（REQ-LAYER-011） |
| ネットワークを開く | ✅ | レイヤーのダブルクリック（ヘッダー/バー）でノードエディタへ |
| レイヤー展開 (▶/▼) | ✅ | プロパティグループ・チャンネル行の開閉 |
| Solo/Mute/Lock トグル | ✅ | Document 更新（solo/mute は Structural 再評価） |
| レイヤー作成 | ✅ | Layer メニュー（Solid/Shape/Video/Null、テンプレートから生成） |
| レイヤー削除 | ✅ | Delete/Backspace（locked は保護）、Document undo で復元 |
| Document/undo 統合 | ✅ | 追加・削除・並べ替え・トリム・移動すべて Document 単位 undo |
| レイヤーバードラッグ移動 | ✅ | バー本体ドラッグ = start_frame 移動、端 6px = in/out トリム。1 ジェスチャ = 1 undo |
| レイヤー並べ替え | ✅ | ヘッダー縦ドラッグ |
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
| 未選択時プレースホルダ | ✅ | `viewer.no_output` locale キー |
| 再生・スクラブ・タイム同期 | ✅ | PlaybackController が再生/シーク毎に ProjectState へ root comp 評価を要求（latest-wins、ドロップ数カウント）。**音声同期はスコープ外のまま**（TASK-013 残項目、`playback-foundation-plan.md` 参照） |
| GPU テクスチャ共有（ゼロコピー） | 🔲 | 現状は評価ワーカーで 1 回読み戻し → `RenderImage`（BGRA u8）変換して表示。GPUI-CE レンダラとの共有サーフェスは Phase 4 ストレッチ |
| ツールバー（選択/ペン等） | 🔲 | ツールシステム計画で対応 |

評価はバックグラウンドワーカー（root comp は Composition 解像度）。
フレームは共有 `PlaybackPosition`（再生ヘッド位置）に従い、編集中も
一時停止中のフレームを再評価する。latest-wins でスクラブ Change・
再生フレームを間引き、UI スレッドは要求投函のみ。音声同期
（オーディオマスタークロック）は明示的に繰延
（`docs/implementation/playback-foundation-plan.md` のスコープ判断）。

---

## プロジェクト永続化（File メニュー）

**ステータス**: `.ravprj` フォーマット v3（layer-network Phase 4）

| 項目 | 状態 | 備考 |
|------|------|------|
| New / Open / Save / Save As | ✅ | File メニュー配線済み。Save As/Open は GPUI ネイティブダイアログ。未保存時の Save は Save As にフォールスルー |
| Document 全体の保存 | ✅ | manifest.json + document/main.ron（Composition・レイヤー・ネットワーク（subnet 入れ子含む）・キーフレーム・予約フィールド・media_assets、決定的 RON）+ assets/refs.json + settings.toml。保存時に前リビジョンを `.bak` 化 |
| マイグレーション | ✅ | v1→v2→v3 連鎖。v2 以前（graph/main.ron のみ）は平坦 Graph を Document に包み、manifest の解像度/fps で root comp を生成 |
| ID カウンタ前進 | ✅ | ロード時に NodeId/EdgeId/CompId/LayerId カウンタをドキュメント最大 ID 超へ（REQ-LAYER-009） |
| undo 履歴 | ✅ | ロード/New は DocumentStore ごと差し替え（undo ステップにしない） |
| ジャーナル版管理 | ✅ | bincode ジャーナルにヘッダ（magic + version）。旧形式・版不一致は破棄（クラッシュジャーナルは揮発性の方針） |
| 未保存変更ガード | 🔲 | New/Open 時の確認ダイアログなし（v1） |
| 自動保存・ジャーナルリプレイ復元 | 🔲 | REQ-PROJ-002、別計画 |

---

## その他パネル

| パネル | 状態 | 備考 |
|--------|------|------|
| MediaBin | 🔲 | PlaceholderPanel |
| Outliner | 🔲 | PlaceholderPanel |
| Dopesheet | 🔲 | PlaceholderPanel |
| Histogram | 🔲 | PlaceholderPanel |
