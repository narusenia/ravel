# UI 実装状況

各パネルの実装済み挙動・描画要素・未実装項目を記録する。

## Node Graph Editor (`panels/node_editor.rs`)

**ステータス**: TASK-014 Done / TASK-015 Done / TASK-016 Done / TASK-017 Done

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
| Evaluator 連携 | ✅ | ravel-nodes プロセッサ自動登録、グラフ変更時に再登録 |
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

- Blur (300, 100)、Constant (50, 100)、Merge (550, 150)
- エッジ: Blur.output[0] → Merge.input[0]

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
| レイヤー選択連動 | ✅ | Timeline のレイヤー選択で Layer セクション表示 (表示のみ、編集は未接続) |
| スクラブでパラメータ変更 | ✅ | 感度=UI レンジ由来、clamp=hard レンジ。Shift=10x / Cmd=0.1x。PropertyChanged Global → NodeEditorPanel で Graph 更新 |
| クリックでテキスト入力 | ✅ | gpui-component Input（EntityInputHandler 経由）。全選択で開始、Enter/blur で確定・clamp、パース不能は復元。IME 実機確認は未 (#41) |
| Select でパラメータ変更 | ✅ | Enum パラメータ (merge operation 等) |
| undo/redo | ✅ | NodeEditorPanel の UndoStack 経由。**undo 単位=ジェスチャ**（スクラブ中の Change は undo を積まず、ドラッグ終了の Commit で 1 スナップショット） |
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

**ステータス**: AE スタイル Composition/Layer UI (PR #38)

旧 Track/Clip モデルは廃止済み。現行タイムラインは `Composition` + `Layer`
（`ravel-core/src/composition/`）を表示する。

### 描画要素

| 要素 | 状態 | 詳細 |
|------|------|------|
| ルーラー | ✅ | 高さ 24px、MM:SS:FF 形式、ズームに応じたティック間隔適応 |
| レイヤーヘッダー | ✅ | 幅 200px、展開矢印、名前、S/M/L トグルボタン |
| レイヤーバー | ✅ | 角丸 4px、start_frame/duration 反映、名前テキスト |
| プロパティ展開行 | ✅ | Position/Scale/Rotation/Opacity グループ、チャンネルサブ行 |
| キーフレームダイヤ | ✅ | Keyframes チャンネルをレイヤーローカル→Comp 時間へ変換して描画 |
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
| レイヤー選択 (ヘッダー/バークリック) | ✅ | SelectedPropertiesTarget::Layer 発行 → Properties 連動 |
| レイヤー展開 (▶/▼) | ✅ | プロパティグループ・チャンネル行の開閉 |
| Solo/Mute/Lock トグル | ✅ | パネルローカル状態のみ (Document 未接続) |
| Document/undo 統合 | 🔲 | パネルローカルのデモ Composition を保持 |
| レイヤーバードラッグ移動 | 🔲 | |
| キーフレーム編集 | 🔲 | |
| 再生/停止連携 | ✅ | PlaybackController（Space/K/←/→、メニュー）が playhead を駆動。follow トグル（コーナーの F）で表示範囲がページ追従 |

### ファイル構成

| ファイル | 役割 |
|---------|------|
| `ravel-app/src/panels/timeline.rs` | GPUI Panel 実装、canvas 描画、イベントハンドラ |
| `ravel-ui/src/panels/timeline.rs` | ヘッドレス状態 (playhead, scroll, zoom, 選択, 展開, S/M/L) |
| `ravel-core/src/composition/` | Composition, Layer, LayerSource, DAG コンパイル |
| `ravel-app/src/playback.rs` | PlaybackController（Transport + tick ループ、評価要求投函） |
| `ravel-core/src/runtime/playback.rs` | PlaybackClock（フレーム精度、wall-clock マスター） |

### デモデータ

- Background: Solid (0-300f)、Footage A: Media (0-90f)、Footage B: Media (100f 開始, 60f)

### 既知の制約

- パネルはデモ Composition をローカル保持し、Document・評価・永続化・undo と未接続。
- Enum の選択肢値（ブレンドモード等）とソース種別値は識別子を兼ねるため未翻訳
  （セクション名・フィールドラベルは locale 経由）。

---

## Viewer パネル

`crates/ravel-app/src/panels/viewer.rs`

| 項目 | 状態 | 備考 |
|------|------|------|
| FrameBuffer 表示 | ✅ | `ViewerFrame` Global 経由、`img` 要素 + `ObjectFit::ScaleDown`（アスペクト維持・拡大なし） |
| 選択ノード評価 | ✅ | NodeEditor が選択変更時に評価要求を投函、バックグラウンド評価（`EvalService`）の結果を世代フィルタして発行 |
| Geometry 自動ラスタライズ | ✅ | 評価ワーカーの `GpuEvalHooks::finalize` で CPU reference により rasterize（GPU texture Viewer は後続） |
| 未選択時プレースホルダ | ✅ | `viewer.no_output` locale キー |
| 再生・スクラブ・タイム同期 | ✅ | PlaybackController が再生/シーク毎に `EvalContext::frame` 実値で評価要求（latest-wins、ドロップ数カウント）。**音声同期・メディアフレーム表示はスコープ外のまま**（TASK-013 残項目、`playback-foundation-plan.md` 参照） |
| GPU テクスチャ共有（ゼロコピー） | 🔲 | 現状は評価ワーカーで 1 回読み戻し → `RenderImage`（BGRA u8）変換して表示。GPUI-CE レンダラとの共有サーフェスは Phase 4 ストレッチ |
| ツールバー（選択/ペン等） | 🔲 | ツールシステム計画で対応 |

評価はバックグラウンドワーカー（512x512）。フレームは共有
`PlaybackPosition`（再生ヘッド位置）に従い、選択駆動評価も一時停止中の
フレームを再評価する。latest-wins でスクラブ Change・再生フレームを
間引き、UI スレッドは要求投函のみ。音声同期（オーディオマスター
クロック）とデコード済みメディアフレームの表示は明示的に繰延
（`docs/implementation/playback-foundation-plan.md` のスコープ判断）。

---

## その他パネル

| パネル | 状態 | 備考 |
|--------|------|------|
| MediaBin | 🔲 | PlaceholderPanel |
| Outliner | 🔲 | PlaceholderPanel |
| Dopesheet | 🔲 | PlaceholderPanel |
| Histogram | 🔲 | PlaceholderPanel |
