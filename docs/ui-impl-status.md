# UI 実装状況

各パネルの実装済み挙動・描画要素・未実装項目を記録する。

## Node Graph Editor (`panels/node_editor.rs`)

**ステータス**: TASK-014 Done / TASK-015 Not Started

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
| 矩形選択 | 🔲 | TASK-015 |
| エッジ削除 | 🔲 | TASK-015 |
| ノード削除 | 🔲 | TASK-015 |
| undo/redo 連携 | 🔲 | TASK-015 |
| pinch ズーム | 🔲 | TASK-015 |
| ミニマップ | 🔲 | TASK-015 |
| コンテキストメニュー (ノード追加) | 🔲 | TASK-015 |
| グリッドスナップ (ドラッグ中) | 🔲 | TASK-015 |

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

## Timeline (`panels/timeline.rs`)

**ステータス**: TASK-012 In Progress

### 描画要素

| 要素 | 状態 | 詳細 |
|------|------|------|
| ルーラー | ✅ | 高さ 24px、MM:SS:FF 形式、ズームに応じたティック間隔適応 |
| トラックヘッダー | ✅ | 幅 150px、種別ラベル (V/A/E)、名前、[M]/[L] インジケータ |
| クリップ矩形 | ✅ | 角丸 4px、RGBA カラー、名前テキスト (幅 > 40px 時) |
| 再生ヘッド | ✅ | 赤色 2px 縦線、全トラック貫通 |
| 選択ハイライト | ✅ | トラックヘッダー背景色変更 (list_active) |
| クリップ選択枠 | ✅ | 選択クリップに 2px foreground ボーダー |
| ミュートオーバーレイ | ✅ | ミュートトラックに半透明オーバーレイ |
| トラック区切り線 | ✅ | 各トラック下部 1px ボーダー |

### インタラクション

| 操作 | 状態 | 詳細 |
|------|------|------|
| 再生ヘッド移動 (ルーラークリック) | ✅ | クリック位置のフレームに移動 |
| 再生ヘッドスクラブ (ルーラードラッグ) | ✅ | ドラッグで連続追従 |
| 水平スクロール | ✅ | マウスホイール dx、scroll_offset 更新 |
| ズーム (Cmd/Ctrl+スクロール) | ✅ | カーソル位置アンカー、pixels_per_frame [0.1, 50.0] |
| トラック選択 (ヘッダークリック) | ✅ | selected_track 設定、背景色変更 |
| トラック追加 (右クリックメニュー) | ✅ | Video/Audio トラック追加、自動連番 |
| トラック削除 (右クリックメニュー) | ✅ | ヘッダー右クリック → Remove Track |
| クリップ選択 | 🔲 | 状態は存在するが UI 未接続 |
| クリップドラッグ移動 | 🔲 | |
| クリップトリム | 🔲 | ヘッドレス層に trim_clip_start/end あり、UI 未接続 |
| クリップリサイズ | 🔲 | |
| キーボードショートカット | 🔲 | |
| 再生/停止連携 | 🔲 | TASK-013 |
| オーディオ波形表示 | 🔲 | |
| ビデオサムネイル | 🔲 | |
| トラック並び替え | 🔲 | |
| スナップ (磁石) | 🔲 | |

### ファイル構成

| ファイル | 役割 |
|---------|------|
| `ravel-app/src/panels/timeline.rs` | GPUI Panel 実装、canvas 描画、イベントハンドラ |
| `ravel-ui/src/panels/timeline.rs` | ヘッドレス状態 (playhead, scroll_offset, pixels_per_frame, selected_*) |
| `ravel-core/src/timeline/track.rs` | Track, Clip, TrackKind, ClipSource |
| `ravel-core/src/timeline/sequence.rs` | Timeline, TimelineError |
| `ravel-core/src/timeline/id.rs` | TrackId, ClipId |

### デモデータ

- Video 1: Clip A (0-90f, 緑)、Clip B (100-160f, 緑)
- Audio 1: Music (10-160f)

---

## その他パネル

| パネル | 状態 | 備考 |
|--------|------|------|
| Viewport | 🔲 | PlaceholderPanel |
| MediaBin | 🔲 | PlaceholderPanel |
| Properties | 🔲 | PlaceholderPanel |
| Outliner | 🔲 | PlaceholderPanel |
| Dopesheet | 🔲 | PlaceholderPanel |
| Histogram | 🔲 | PlaceholderPanel |
