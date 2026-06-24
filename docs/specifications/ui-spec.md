# Ravel — UI仕様書

## 概要

RavelのUIはGPUI（Zed由来）で構築。ノードグラフファーストの設計思想に基づき、ノードグラフが全プリセットで常時表示される。`gpui_component`のdock/tab機能を基盤に、パネルの自由なドラッグ・タブ統合・引き剥がしが可能。

## 設計原則

- **ノードグラフファースト**: ノードグラフは全プリセットで常時表示。タイムラインはSequenceノードの糖衣UI
- **Sequenceノード糖衣モデル**: NLE的タイムライン編集は特殊ノード「Sequence」のUI表現。DAGの外に別概念を持たない
- **Timelineの二面性**: Sequenceノード選択時はトラック/クリップエディタ（NLE的UI）、それ以外はドープシート/カーブエディタ
- **Outliner**: ジェネレータ起点の自動オブジェクトリスト + サブグラフ折りたたみ。AEのレイヤーリストに相当
- **パネルのタブ統合**: 任意のパネルをドラッグで別パネルにタブ統合可能（gpui-component DockArea）

## シーンモデル

Ravelは「シーン」という別概念を導入しない。**ノードグラフ自体がシーン定義**。

```
Root Graph (= シーン)
├── Shape(circle, repeat:12)     ← プロシージャル生成
├── TextGenerator("Title")       ← モーグラ
├── Read("clip_01.mov")          ← 実写素材
├── Sequence                     ← タイムライン糖衣ノード
│   ├─ V1: [clip_01 | T | clip_02]
│   └─ A1: [bgm.wav]
├── ColorCorrect
├── Merge
└── Write("output.mp4")
```

### サブグラフ（2種）

| 種類 | 用途 | コンテキスト |
|------|------|-------------|
| **Group** | ノードの整理用グループ | 親と同じ解像度/FPS/尺で評価。入出力ポートで外と接続 |
| **Comp** | 独立コンポジション（AEプリコンプ相当） | 独自の解像度/FPS/尺を持つ。別世界として評価 |

- 親グラフでは両方とも1ノードとして表示（入出力ポート付き）
- ダブルクリックで中に潜る（ブレッドクラム表示）
- Ctrl+G で選択ノードをGroup化

### ワークフロー別の流れ

**モーグラ（メイン）**:
1. ノードグラフでShape/Text/Particle等を配置・接続
2. パラメータにキーフレーム → Dopesheet/Curve Editorで調整
3. Sequenceノード不要。ノードグラフ=作品

**VFX**:
1. Readノードで素材を読み込み
2. ノードグラフでKeying/Tracking/Merge等を接続
3. 必要ならSequenceノードで複数ショットをタイムライン上に並べる

**初心者/カット編集**:
1. 新規プロジェクト → テンプレートがSequenceノード付きで開始
2. Timelineパネルにメディアドロップ → Readノード自動生成+Sequence配置
3. カット/トリミングはTimeline上で完結
4. 「ノードグラフで開く」で裏のDAGが見える → 学習の入口

## パネル一覧

| パネル | 説明 | デフォルト表示 |
|--------|------|---------------|
| Outliner | ジェネレータ起点のオブジェクトリスト + サブグラフ折りたたみ | Edit, Node, Motion |
| Node Graph Editor | ノードグラフの編集。全プリセットで常時表示 | 全プリセット |
| Timeline | Sequence選択時: トラック/クリップエディタ。それ以外: ドープシート | Edit |
| Viewer | プレビュー表示 | 全プリセット |
| Dopesheet | キーフレーム打点の一覧。Curve Editorとタブ共存 | Node, Motion |
| Curve Editor | アニメーションカーブ編集。Dopesheetとタブ共存 | Node, Motion |
| Properties Inspector | 選択ノードのパラメータ編集 | Edit, Node, Motion |
| Media Bin | プロジェクトメディア管理（サムネ一覧） | Edit |
| Scopes (Waveform) | 波形モニタ | Color |
| Scopes (Vectorscope) | ベクトルスコープ | Color |
| Scopes (Histogram) | ヒストグラム | Color |
| Scopes (Parade) | パレード | Color |
| Text Editor | タイポグラフィ編集 | Motion |
| Render Queue | レンダージョブ管理 | （手動表示） |
| Shader Editor | WGSLカスタムシェーダ編集 | （手動表示） |
| Lua Console | スクリプトエディタ/コンソール | （手動表示） |

## ワークスペースプリセット

全プリセット共通: ノードグラフは常時表示。パネルはドラッグでタブ統合・引き剥がし可能。

### Edit

初心者/カット編集の入口。Sequenceノード中心のワークフロー。

```
┌───────────────┬───────────────────┬────────────┐
│ [Outliner]    │                   │            │
│ [Media Bin]   │      Viewer       │            │
│   ↑タブ切替   │                   │ Properties │
├───────────────┼───────────────────┤            │
│               │                   │            │
│  Node Graph   │ Timeline(Sequence)│            │
│               │                   │            │
└───────────────┴───────────────────┴────────────┘
```

### Node

プロシージャル/VFXワークフロー。ノードグラフが最大面積。

```
┌───────────────┬───────────────────┬────────────┐
│               │                   │            │
│   Outliner    │      Viewer       │ Properties │
│               │                   │            │
├───────────────┴───────────────────┤            │
│                                   │            │
│           Node Graph              │            │
│                                   │            │
├───────────────────────────────────┤            │
│ [Dopesheet] [Curve Editor]        │            │
└───────────────────────────────────┴────────────┘
```

### Color

カラーグレーディングワークフロー。Viewer+スコープ4種が主役。

```
┌───────────────────────┬────────────┐
│                       │  Waveform  │
│        Viewer         ├────────────┤
│                       │Vectorscope │
├───────────────────────┼────────────┤
│                       │ Histogram  │
│      Node Graph       ├────────────┤
│                       │   Parade   │
├───────────────────────┴────────────┤
│ [Dopesheet] [Curve Editor]         │
└────────────────────────────────────┘
```

### Motion

モーショングラフィックス/リリックモーション制作。テキスト編集とカーブ調整を重視。

```
┌───────────────┬───────────────────┬────────────┐
│               │                   │    Text    │
│   Outliner    │      Viewer       │   Editor   │
│               │                   │            │
├───────────────┴───────────────────┤────────────┤
│                                   │            │
│           Node Graph              │ Properties │
│                                   │            │
├───────────────────────────────────┴────────────┤
│ [Dopesheet] [Curve Editor]                     │
└────────────────────────────────────────────────┘
```

## Outliner詳細

ジェネレータノード（入力を持たないソースノード: Shape, Text, Read等）を自動でトップレベルに列挙。各オブジェクトから下流の処理チェーンを展開表示可能。サブグラフ（Group/Comp）は折りたたみグループとして表示。

```
Outliner                            Node Graph
─────────                           ──────────
▼ ● circle_array (Shape)           [Shape]──[Repeat]──┐
    Repeat ×12                                         ├──[Merge]──[Write]
    ColorCorrect                                       │
▼ ● title (Text)                   [Text]──[Animate]──┘
    Animate
▶ ● bgm (Read)                    [Read]──[Sequence.A1]
▼ ● edit_sequence (Sequence)
    V1: clip_01, clip_02
    A1: bgm
```

- Outlinerでの選択 → Node Graphで対応ノードをフォーカス
- Node Graphでの選択 → Outlinerで対応エントリをハイライト
- 双方向同期

## ノードグラフエディタ詳細

### インタラクション

- **パン**: 中ボタンドラッグ / Space+左ドラッグ
- **ズーム**: スクロールホイール / ピンチ
- **ノード選択**: 左クリック / 矩形選択
- **ノード移動**: 選択後ドラッグ（スナップガイド表示）
- **接続**: ポートからドラッグ → 互換ポートにドロップ（型互換性をリアルタイム表示）
- **ノード追加**: ダブルクリック / Tab → 検索パレット表示
- **Group化**: 複数選択 → Ctrl+G
- **Group展開**: ダブルクリック → 中身に潜る（ブレッドクラム表示）

### ノード検索パレット

Tab押下またはキャンバスダブルクリックでポップアップ表示。

- テキスト入力でインクリメンタル検索
- カテゴリ別フィルタ（Color/Transform/Generate/Effect/...）
- 最近使用したノードを優先表示
- ドラッグ中のワイヤーから発動した場合、接続可能な型のノードのみ表示

### ノード表示

```
┌─────────────────────────┐
│  ● Gaussian Blur        │  ← ヘッダ（タイプ名/ラベル、色付き）
├─────────────────────────┤
│  ○ Image    ▸ Result ○  │  ← 入出力ポート（型別色分け）
├─────────────────────────┤
│  Radius     [■■■■□] 5.0 │  ← インラインパラメータ
│  Quality    [High ▾]    │
└─────────────────────────┘
```

## Timelineの二面性

### Sequenceモード（Sequenceノード選択時）

```
┌────────────────────────────────────────────────────────┐
│ ◁ ▶ ■ │ 00:01:23:15 │ ♪ BPM:128 │ ▸ markers ▾       │ ← トランスポート
├────────┬───────────────────────────────────────────────┤
│        │  |    |    |    |    |    |    |    |    |    │ ← タイムルーラー
│        │  ▼ ▼  ▼        ▼                  ▼          │ ← ビートマーカー
├────────┼───────────────────────────────────────────────┤
│ V1  👁 │ [  clip_01.mov  ] [T] [  clip_02.mov  ]     │ ← ビデオトラック
│        │                  ↕                            │ ← トランジション
├────────┼───────────────────────────────────────────────┤
│ V2  👁 │     [ Title Text (Motion) ]                  │ ← タイトル
├────────┼───────────────────────────────────────────────┤
│ A1  🔊 │ [ ♪ bgm.wav                              ]  │ ← オーディオトラック
│        │  ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~          │ ← 波形表示
└────────┴───────────────────────────────────────────────┘
```

- クリップのエフェクトスタックが直列チェーンの間はタイムラインUI上で順序変更・パラメータ編集可能
- 右クリック → 「ノードグラフで開く」で対応するノードチェーンがノードグラフに展開
- 分岐が入った場合、タイムライン上では「カスタムエフェクト」として表示

### Dopesheet/Curve Editorモード（通常）

選択中のノードのプロパティキーフレームを表示。Dopesheet（打点一覧）とCurve Editor（カーブ編集）はタブ切替。

## ビューア詳細

### コントロール

- 再生/停止: Space
- フレーム送り/戻し: ←/→
- 先頭/末尾: Home/End
- ズーム: Ctrl+スクロール
- フィット: F
- 表示ノード切替: ノードグラフでAlt+クリック

### スコープ

メニュー「表示」から各スコープのトグル。各スコープはOCIOカラースペースを反映。

## テーマシステム

### テーマ定義 (TOML)

```toml
[meta]
name = "Ravel Dark"
author = "Ravel Team"
variant = "dark"              # dark | light
color_vision = "normal"       # normal | protanopia | deuteranopia | tritanopia

[colors]
background = "#1e1e2e"
surface = "#2a2a3c"
primary = "#7c6ff0"
secondary = "#f07c6f"
text = "#e0e0e0"
text_secondary = "#808090"
border = "#3a3a4c"
error = "#f04040"
warning = "#f0c040"
success = "#40c040"

[colors.node_types]
image = "#4a90d9"
color = "#d9a54a"
generate = "#4ad98a"
transform = "#d94a90"
audio = "#90d94a"
text = "#d9d94a"

[colors.scopes]
waveform = "#40ff40"
vectorscope = "#ffffff"
```

## キーバインド

TOML定義。各セクションはテーブル。キー=コマンドアクション、値=キーコード。コマンドid = `<section>.<action>` で `ravel_ui::command::CommandId` と一致必須。

- `[meta]` セクションにプリセット名/作者を記述
- 修飾子トークン: `Cmd`（Super/Meta/Win）= プラットフォーム主修飾。`Ctrl` は物理Controlキー。加えて `Shift` / `Alt`(Option)。チョードは `+` 区切りで修飾子先頭・キー末尾

```toml
[meta]
name = "Ravel Default"
author = "Ravel Team"

[file]
new = "Cmd+N"
open = "Cmd+O"
save = "Cmd+S"
save_as = "Cmd+Shift+S"
quit = "Cmd+Q"

[edit]
undo = "Cmd+Z"
redo = "Cmd+Shift+Z"
cut = "Cmd+X"
copy = "Cmd+C"
paste = "Cmd+V"

[view]
toggle_timeline = "Alt+1"
toggle_node_graph = "Alt+2"
toggle_viewer = "Alt+3"
toggle_properties = "Alt+4"
toggle_curve_editor = "Alt+5"
toggle_scopes = "Alt+6"

[workspace]
edit = "Cmd+F1"
node = "Cmd+F2"
color = "Cmd+F3"
motion = "Cmd+F4"

[panel]
detach = "Cmd+Shift+D"
reattach = "Cmd+Shift+R"

[help]
about = "F1"
```

> `[timeline]` / `[node_graph]` 等のドメイン別セクションは後続タスク（TASK-012 タイムライン、TASK-014/015 ノードグラフ）でコマンドid追加時に同形式で拡張する。

## 制約・前提条件

- GPUIの制約としてネイティブメニューバーの挙動がOS間で異なる可能性
- GPUI 0.2.2 の `gpui::MenuItem::Action` に checked/checkbox variant が存在しないため、ネイティブメニューのチェックマーク表示は未対応。ヘッドレスモデル層（`ravel_ui::menu`）では正しく追跡済。カスタムメニュー描画（`gpui_component::PopoverMenu`）で将来対応予定
- タブグルーピング（`[Outliner] [Media Bin] ↑タブ切替` 等）: `LayoutNode` に `Tab` variant が未実装のため、プリセットレイアウトでは片方のパネルのみ配置。`LayoutNode::Tabs` 追加で対応予定
- フリードッキングの実装は`gpui_component`のdock機能の成熟度に依存
- スクリーンリーダー完全対応はGPUIのカスタムレンダリング特性上、テキスト要素に限定
- 関連要件: REQ-UI-001〜010
