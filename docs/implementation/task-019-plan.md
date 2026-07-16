# TASK-019: Composition/Layer モデル移行（AEモデル）

## Context

Ravel のタイムラインモデルを NLE（Premiere/DaVinci）型の Track/Clip ベースから、
AE/Cavalry 型の Composition/Layer ベースに全面移行する。

これは要件定義書 REQ-CORE-001 v2、REQ-CORE-008 v2、REQ-UI-003 v2 に基づく。

依存: TASK-016 (ビルトインノード ✅), TASK-017 (NodeEditor Interaction ✅)

## 設計決定サマリ

| 項目 | 決定 |
|------|------|
| モデル | フル AE モデル（Track/Clip 廃止 → Composition/Layer） |
| 合成順序 | 下から上（AE 標準） |
| Layer ソース | 7種: Media, Solid, Shape, Text, PreComp, Generator, Null |
| Transform | Layer にビルトイン（position/scale/rotation/opacity/anchor_point） |
| エフェクト | ノードサブグラフ（直列=スタックUI、分岐=ノードグラフUI） |
| Comp-Node 関係 | CompNode = DAG 上の特殊ノード（コンパイラ方式で展開） |
| Parenting | 初期からサポート（parent: Option<LayerId>） |
| BlendMode | 基本5種（Normal/Add/Multiply/Screen/Overlay） |
| タイムライン UI | AE 型: レイヤーバー + ▼プロパティ展開 + キーフレーム菱形 |
| キーフレーム | 既存 AnimationChannel をそのまま活用 |
| 評価方式 | コンパイラ方式（Fable 推奨）— Evaluator 変更不要 |
| Composition 保存 | ドキュメント層に `im::HashMap<CompId, Arc<Composition>>` |
| Alpha 規約 | premultiplied alpha で統一 |

## スコープ

### Phase 1: データモデル（ravel-core）

#### 1a. Composition/Layer/CompId 型定義

```
crates/ravel-core/src/composition/
├── mod.rs       # Composition, Layer, LayerId, CompId, LayerSource, BlendMode
├── compile.rs   # Composition → DAG 展開（コンパイラ）
└── validate.rs  # PreComp 循環検出、Layer parenting 循環検出
```

- `CompId`: `NodeId` と同様の原子カウンタ ID
- `LayerId`: 同上
- `Composition`: `im::Vector<Layer>` で構造共有、イミュータブル操作
- `Layer`: ビルトイン Transform を `AnimChannel` で保持
- `LayerSource`: 7 バリアント
- `BlendMode`: 5 バリアント

#### 1b. 旧 Timeline モジュール廃止

- `ravel-core/src/timeline/` を削除（sequence.rs, track.rs, id.rs）
- `ravel-core/src/lib.rs` から `pub mod timeline` を削除
- 依存箇所（ravel-ui の TimelinePanel ヘッドレス状態）を更新

#### 1c. Composition コンパイラ

`compile.rs`: Composition の Layer 群を DAG ノード列に展開する関数。

```rust
pub fn compile_composition(
    comp: &Composition,
    comp_node_id: NodeId,
    graph: &Graph,
    id_alloc: &mut impl FnMut() -> NodeId,
    edge_alloc: &mut impl FnMut() -> EdgeId,
) -> Graph
```

各 Layer について:
1. `LayerSource` に応じたソースノードを生成
2. `effect_graph` があればサブグラフを接続
3. Transform ノードを生成（Layer の position/scale/rotation を適用）
4. Opacity 適用ノードを生成
5. `BlendMode` に応じた Merge ノードで下のレイヤーと合成
6. solo/muted の処理（ミュートレイヤーはスキップ、solo 時は非 solo をスキップ）
7. Parent チェーンの Transform 継承を解決（ワールド行列計算）

展開結果は通常の Graph に挿入され、CompNode の出力ポートに最終 Merge の出力を接続。

### Phase 2: CompNode プロセッサ（ravel-nodes）

- `CompNodeProcessor`: `comp_id` パラメータから Composition を取得し、コンパイラで展開
- 展開結果を内部 Graph に保持し、Evaluator で評価
- Composition 変更時に再展開（dirty フラグで検出）

### Phase 3: タイムライン UI 書き直し（ravel-app）

#### 3a. ヘッドレス状態（ravel-ui）

```
crates/ravel-ui/src/panels/timeline.rs  # 全面書き直し
```

- `TimelinePanel` → Composition の Layer リスト表示状態
- 選択中の CompId、展開中のプロパティ、プレイヘッド位置

#### 3b. GPUI パネル（ravel-app）

```
crates/ravel-app/src/panels/timeline.rs  # 全面書き直し
```

**左パネル（Layer リスト）:**
- Layer 名、solo/mute/lock ボタン
- ▼ でプロパティ展開（Position, Scale, Rotation, Opacity）
- ドラッグで合成順序変更

**右パネル（時間軸）:**
- ルーラー（流用: MM:SS:FF）
- プレイヘッド（流用: 赤い縦線 + スクラブ）
- Layer バー: in/out 範囲を表す水平バー
  - 左端ドラッグ: in トリム
  - 右端ドラッグ: out トリム
  - 中央ドラッグ: スリップ（start_frame 変更）
- キーフレーム菱形: プロパティ行にキーフレーム位置を表示
- ズーム/パン（流用: Cmd+scroll, scroll）

### Phase 4: 統合 + Properties パネル連動

- Composition/Layer 選択時に Properties パネルに Layer プロパティを表示
- `PropertySection` に Layer 用セクション生成を追加
- CompNode 選択時のノードグラフ ↔ タイムライン連動

---

## コミット計画

| # | Phase | 内容 |
|---|-------|------|
| 1 | 1a | feat: add Composition/Layer/CompId types to ravel-core |
| 2 | 1b | refactor: remove legacy Timeline/Track/Clip module |
| 3 | 1c | feat: implement Composition compiler (Layer → DAG flatten) |
| 4 | 1c | test: add compilation tests for all LayerSource types |
| 5 | 2 | feat: implement CompNodeProcessor with compilation |
| 6 | 3a | feat: rewrite TimelinePanel headless state for Composition model |
| 7 | 3b | feat: implement AE-style timeline UI with layer bars |
| 8 | 3b | feat: add keyframe diamond rendering on timeline |
| 9 | 3b | feat: add layer property expansion (▼ Position/Scale/Rotation) |
| 10 | 4 | feat: wire Composition timeline with Properties panel |
| 11 | — | docs: update requirements and specifications |

---

## 検証

- `cargo build` — 全クレート警告なし
- `cargo test -p ravel-core` — Composition/Layer テスト + コンパイラテスト
- `cargo test -p ravel-nodes` — CompNodeProcessor テスト
- `cargo test -p ravel-ui` — 既存テスト通過（Timeline 関連は更新）
- `RUSTFLAGS="-D warnings" cargo clippy` — clean
- `cargo fmt --check` — clean
- UI 手動テスト: Layer 追加/削除/並べ替え、in/out トリム、キーフレーム表示
- UI 手動テスト: PreComp（入れ子 Composition）動作確認

## リスク

- **AnimationChannel の統合**: Layer の Transform プロパティに AnimChannel を組み込む際、既存の Channel 実装と Layer のフレーム座標系（Comp ローカル vs ソースローカル）の整合が必要
- **コンパイラの再展開コスト**: Layer 追加/削除時にグラフ全体の再展開が走る。大規模 Composition ではパフォーマンス影響あり → 差分展開の最適化は後続タスク
- **既存デモグラフ**: NodeEditorPanel の `build_demo_graph` がCompNode を使ったデモに差し替えが必要
