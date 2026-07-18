# TASK-017: NodeEditor Interaction 強化 + Properties パネル

## Context

TASK-014/015 でノードエディタ UI、TASK-016 でビルトインノードプロセッサが完成。
TASK-017 では ravel-nodes との連携層を確立し、Properties パネルによるパラメータ編集、
コンテキストメニュー充実、コネクション描画スタイル切替、Copy/Paste、ポート型フィルタリングを実装する。

依存: TASK-015 (ノードエディタ UI ✅), TASK-016 (ビルトインノードプロセッサ ✅)

## 前提（実装済みインフラ）

### NodeEditorPanel (panels/node_editor.rs)
- Graph + UndoStack + NodeRegistry 管理
- ノード選択 (HashSet<NodeId>)、エッジ選択 (HashSet<EdgeId>)
- Delete/Backspace キーでノード・エッジ削除（undo対応）
- 右クリックコンテキストメニュー: Add Node サブメニュー, Delete Edge
- 接続ドラッグ、選択ボックス、ズーム/パン
- FocusedPanelGlobal / PanelUndoRedo による Global シグナル通信

### ravel-nodes (TASK-016)
- 5 プロセッサ: ConstantProcessor, ColorCorrectProcessor, BlurProcessor, TransformProcessor, MergeProcessor
- `register_all_processors(evaluator, graph, ctx, shaders)` 関数
- ravel-app の Cargo.toml に依存追加済み、コード上の呼び出しは未実装

### gpui-component
- Accordion, Slider, NumberInput, Input, Switch, Select, ColorPicker, DescriptionList, Settings 等のフォーム UI が利用可能

### ravel-ui (headless)
- `PropertiesPanel` 状態 (Selection enum: Empty/Node/Clip) が `ravel-ui/src/panels/properties.rs` に存在
- PanelKind::Properties はワークスペースに PlaceholderPanel として登録済み

---

## スコープ

### Phase 0: ravel-nodes 連携層

NodeEditorPanel に Evaluator + GpuContext + ShaderManager を統合。
グラフ変更 → `register_all_processors` → 評価のフローを確立。

```rust
// NodeEditorPanel に追加
evaluator: Evaluator,
gpu_ctx: GpuContext,
shader_manager: ShaderManager,
```

- ノード追加/削除/パラメータ変更時に `register_all_processors` を再呼び出し
- 評価結果は将来の Viewer パネルで表示（本タスクでは評価フロー確立まで）

### Phase 1: Properties パネル

#### 1a. データモデル (ravel-ui)

`ravel-ui/src/properties/` に汎用プロパティシステムを実装。

```rust
// PropertyField — 7バリアント
pub enum PropertyField {
    Float {
        key: String,
        value: f32,
        range: Option<RangeInclusive<f32>>,
        step: Option<f32>,
    },
    Int {
        key: String,
        value: i32,
        range: Option<RangeInclusive<i32>>,
        step: Option<i32>,
    },
    Bool { key: String, value: bool },
    String { key: String, value: String },
    Enum {
        key: String,
        value: String,
        options: Vec<String>,
    },
    Color { key: String, r: f32, g: f32, b: f32, a: f32 },
    ReadOnly { key: String, value: String },
}

// PropertySection trait
pub trait PropertySection {
    fn title(&self) -> &str;
    fn fields(&self) -> Vec<PropertyField>;
}
```

#### 1b. ノード用セクション生成 (ravel-ui)

```rust
// ravel-ui/src/properties/node.rs
pub struct NodeInfoSection { ... }    // type_key, label (ReadOnly)
pub struct NodeParamsSection { ... }  // parameters → Float/Int/Bool/String/Enum

impl PropertySection for NodeInfoSection { ... }
impl PropertySection for NodeParamsSection { ... }
```

- merge ノードの `operation` パラメータ → Enum (over/add/multiply)
- その他の Float/Int/Bool/String はそのまま対応するフィールド型へ

#### 1c. Global シグナル通信

```rust
// 選択通知: NodeEditorPanel → PropertiesPanel
pub struct SelectedPropertiesTarget(pub PropertiesTarget);
impl Global for SelectedPropertiesTarget {}

pub enum PropertiesTarget {
    Empty,
    Nodes {
        ids: Vec<NodeId>,
        nodes: Vec<Arc<Node>>,
    },
    // 将来: Clip, Project
}

// 変更通知: PropertiesPanel → NodeEditorPanel
pub struct PropertyChanged {
    pub target: PropertyChangeTarget,
    pub key: String,
    pub value: PropertyValue,
}
impl Global for PropertyChanged {}
```

#### 1d. PropertiesGpuiPanel (ravel-app)

`PanelKind::Properties` のプレースホルダーを実体に差し替え。

- `observe_global::<SelectedPropertiesTarget>` で選択変更を受信
- セクション群を `Accordion` で描画
- 各フィールド型 → gpui-component ウィジェットへマッピング:

| PropertyField | ウィジェット |
|--------------|------------|
| Float | Slider + NumberInput |
| Int | NumberInput (step付) |
| Bool | Switch |
| String | Input |
| Enum | Select |
| Color | ColorPicker |
| ReadOnly | DescriptionList / テキスト表示 |

- 値変更時: `cx.set_global(PropertyChanged { ... })`
- 複数ノード選択時: 同型ノードなら一括編集、異なる値は `---` 表示

#### 1e. undo/redo 統合

- PropertyChanged を NodeEditorPanel が `observe_global` で受信
- Graph 更新 → UndoStack commit
- 更新後に SelectedPropertiesTarget を再発火 → Properties 再描画

### Phase 2: コンテキストメニュー充実 + エッジ削除

- 右クリック「Delete Node」追加（選択ノード対象）
- **Delete キーでエッジも削除**: 選択中のエッジを Delete/Backspace で削除可能にする
  - 現状ノード削除のみ対応 → エッジ選択状態でも同じキーで削除
- bypass/dissolve: 中間ノード除去、前後の接続を維持
  - 入力1つ・出力1つのノードのみ対象
  - 前ノードの出力ポートと後ノードの入力ポートを直結
  - undo 対応

### Phase 3: コネクションスタイル切替

3モード: Bezier / Straight / Step (直角折れ線)

- グローバル設定で一括切替（エッジごとではない）
- NodeEditorPanel に `edge_style: EdgeStyle` フィールド追加
- `painting.rs` の `paint_edges` で EdgeStyle に応じて描画分岐
- ヒットテストも EdgeStyle に応じた距離計算
- 右クリックメニューまたはツールバーから切替

```rust
pub enum EdgeStyle {
    Bezier,    // 現行の S 字カーブ
    Straight,  // 直線
    Step,      // 水平→垂直の直角折れ線
}
```

### Phase 4: Copy/Paste + Duplicate

- **Copy** (Cmd+C): 選択ノード群 + 内部エッジをクリップボードに保存
  - 内部データ構造としてシリアライズ（OS クリップボードではなくアプリ内）
  - ノード座標は相対オフセットで保持
- **Paste** (Cmd+V): クリップボードから新規ノード群を生成
  - 新規 NodeId/EdgeId を割り当て
  - ペースト位置はマウスカーソル位置 or 選択ノードの右下にオフセット
  - 外部接続は破棄（内部エッジのみ復元）
- **Duplicate** (Cmd+D): Copy + Paste を1操作で実行
  - (20, 20) px オフセットで配置

### Phase 5: ポート型フィルタリング + 単一入力制約

- **単一入力ポート制約**: 1つの入力ポートに複数エッジを接続できないようにする
  - 接続ドラッグ中、既に接続済みの入力ポートへのスナップを拒否
  - 既存接続がある入力ポートに接続した場合、既存エッジを自動的に置換（or 拒否）
  - `Graph::add_edge` 時に同一 target+target_port のエッジが既存なら除去してから追加
- 接続ドラッグ開始時、非互換ポートを暗転表示
  - InputPort の `accepted_types` と OutputPort の `data_type` を照合
- 非互換ポートへのドロップを拒否（エッジ作成しない）
- `painting.rs` のポート描画で「互換/非互換」に応じてアルファ値を変更

### Phase 6: バグ修正 + UX 改善

- **Workspace 切替時のグラフリセット修正**: ワークスペースプリセット切替時に NodeEditorPanel が再生成され、デモグラフに戻る問題を修正
  - Graph をパネル間で共有するか、DockArea の状態復元時にグラフを保持する仕組みが必要
  - 最低限: グラフ状態を Global または永続ストアに退避し、パネル再生成時に復元
- **値スナップ機能**: Slider でのパラメータ編集時に値をスナップ
  - ステップ値に応じたスナップ (step=0.01 なら小数2桁)
  - Shift ドラッグで粗いステップ (step × 10)
  - Ctrl クリックで手入力モード切替
- **ミニマルスライダー自前実装** (将来): gpui-component の Slider は丸い Thumb が大きく場所を取る。Canvas ベースの薄いスライダー（Thumb なし、バー + ハイライト区間のみ）を自前実装してよりコンパクトに
- **Fit View (全体表示)**: 全ノードが画面に収まるようズーム+パン調整
  - `Viewport::fit_nodes()` は既に実装済み（未接続）→ キーバインド (F) またはコンテキストメニューで呼び出し
  - ノードを見失った時に使うリカバリ操作

### Phase 7: パラメータの InputPort 化（ノード駆動パラメータ）

> **2026-07-18**: 本フェーズは `param-input-ports-plan.md` として独立計画に
> 昇格した（設計合意済み。実 InputPort + 実エッジ方式 — 下記の
> `parameter_inputs` 別フィールド案は dirty 伝播が届かないため不採用）。
> 以下は当時の原案として残す。

- **概要**: 各ノードのパラメータを隠し InputPort として公開し、他ノードの出力をエッジで接続してパラメータ値を動的に駆動する
- **Node 構造変更**: `parameter_inputs: Vec<Option<(NodeId, OutputPortIndex)>>` を追加（or パラメータごとに対応 InputPort を自動生成）
- **Evaluator 変更**: `process` 呼び出し前に parameter_inputs を解決し、接続元ノードの出力値でパラメータを上書き
- **UI**: ノードエディタ上でパラメータポートを表示（小さいドットをパラメータ行の左に配置）、エッジ接続可能に
- **型変換**: 出力型と ParameterValue 型の変換ルール（Scalar→Float, etc.）
- **Properties パネル連携**: 接続済みパラメータは値表示をグレーアウトし「connected」表示

### Phase 8: アセット・アイコン追加

- **NodeGraph ツールバー**: パネルヘッダー下にツールバーUI追加（EdgeStyle切替ボタン、Fit View ボタン等）
- **アイコンアセット**: ツールバー用アイコン SVG/PNG を `assets/icons/` に追加
- **ノードカテゴリアイコン**: Generator / Filter / Compositor / Transform / Color のカテゴリアイコン

---

## ファイル構成（新規・変更）

```
crates/ravel-ui/src/properties/
├── mod.rs              # PropertySection trait, PropertyField enum, PropertiesTarget
├── node.rs             # NodeInfoSection, NodeParamsSection
└── (project.rs)        # 将来: プロジェクト設定セクション

crates/ravel-app/src/panels/
├── properties.rs       # PropertiesGpuiPanel (新規、PlaceholderPanel 差替)
└── node_editor.rs      # Phase 0 連携層追加、PropertyChanged observer、bypass 等

crates/ravel-app/src/node_editor/
├── painting.rs         # EdgeStyle 対応、ポート型フィルタリング描画
├── bezier.rs           # → edge_style.rs にリネーム or EdgeStyle 対応追加
└── clipboard.rs        # (新規) Copy/Paste 用のクリップボード

crates/ravel-app/src/workspace.rs  # PropertiesGpuiPanel 登録差替
```

---

## コミット計画

| # | Phase | 内容 |
|---|-------|------|
| 1 | 0 | feat: integrate ravel-nodes evaluator into NodeEditorPanel |
| 2 | 1a | feat: add PropertySection trait and PropertyField types to ravel-ui |
| 3 | 1b | feat: implement node property section generators |
| 4 | 1c | feat: add SelectedPropertiesTarget and PropertyChanged globals |
| 5 | 1d | feat: implement PropertiesGpuiPanel with Accordion layout |
| 6 | 1e | feat: wire Properties ↔ NodeEditor undo/redo integration |
| 7 | 2 | feat: add Delete Node and bypass/dissolve to context menu |
| 8 | 3 | feat: add Bezier/Straight/Step edge style switching |
| 9 | 4 | feat: implement node Copy/Paste and Duplicate |
| 10 | 5 | feat: add port type filtering during connection drag |
| 11 | 6 | fix: preserve graph state across workspace preset switches |
| 12 | 6 | feat: add Fit View keybinding and context menu entry |
| 13 | — | docs: update task-017 plan and ui-impl-status |

---

## 検証

- `cargo build` — 全クレート警告なし
- `cargo test -p ravel-ui` — PropertySection/PropertyField テスト
- `cargo test -p ravel-nodes` — 既存16テスト通過
- `cargo test -p ravel-app` — 統合テスト（あれば）
- `RUSTFLAGS="-D warnings" cargo clippy` — clean
- `cargo fmt --check` — clean
- UI 手動テスト: Properties パネルでパラメータ変更 → undo/redo 動作確認
- UI 手動テスト: コネクションスタイル切替、Copy/Paste、bypass/dissolve
