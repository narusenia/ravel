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
    comp_id: CompId,
    graph: Graph,
) -> CompilationResult
```

**決定論的 ID 割り当て** (Fable 指摘 #2 対処):
- 展開で生成されるノード/エッジの ID は `(CompId, LayerId, Role)` から決定論的に導出
- `Role`: Source=0, Effects=1, Transform=2, Opacity=3, Merge=4, TimeOffset=5
- ID 計算: `NodeId::new(comp_id.raw() << 32 | layer_id.raw() << 8 | role)`
- 再展開時に同一 ID が再利用される → Evaluator のキャッシュが維持される

**Synthetic ノードフラグ** (Fable 指摘 #1 対処):
- 展開で生成されたノードは `Node.metadata.synthetic = true` でマーク
- 永続化(.ravprj)時に synthetic ノードは除外
- ノードエディタUI では synthetic ノードは非表示（or 半透明で参考表示）
- Undo スナップショットでは Graph + CompMap を統一ドキュメントとして管理

各 Layer について:
1. `LayerSource` に応じたソースノードを生成
2. **TimeOffset ノード生成** (Fable 指摘 #3 対処): `start_frame` と `[in, out)` に基づいて
   EvalContext.frame をオフセットする専用ノード。PreComp の場合は子 Comp の fps への変換も行う
3. `effect_graph` があればサブグラフを接続
4. **Parent Transform 解決** (Fable 指摘 #4 対処): 親 Layer の Transform ノード出力を
   子 Layer の Transform ノード入力に接続（評価時エッジ）。コンパイル時の行列計算ではなく、
   DAG のエッジとして表現し Evaluator が自然に解決する
5. Transform ノードを生成（Layer の position/scale/rotation を適用）
6. Opacity 適用ノードを生成
7. `BlendMode` に応じた Merge ノードで下のレイヤーと合成
8. solo/muted の処理: 展開前のプレパスで active layer リストを決定
   - muted → スキップ（ただし children が参照する場合は Transform のみ残す）
   - solo → 非 solo をスキップ

展開結果はメイングラフに挿入。CompNode の出力ポートに最終 Merge の出力を接続。

### Phase 2: CompNode プロセッサ + TimeOffset ノード（ravel-nodes）

- `CompNodeProcessor`: Composition 変更を検出し、コンパイラで再展開をトリガー
  - 構造変更（Layer 追加/削除/順序変更）→ 再展開
  - キーフレーム変更のみ → 再展開不要（dirty 通知のみ、ID が安定しているためキャッシュ有効）
- `TimeOffsetProcessor`: EvalContext.frame を変換する CPU ノード
  - `frame' = (frame - start_frame).clamp(in_frame, out_frame - 1)`
  - PreComp の場合: `frame' = frame * (child_fps / parent_fps)` + offset
- **Undo 統一** (Fable 指摘 #8): Graph と `im::HashMap<CompId, Arc<Composition>>` を
  1つの `Document` 構造体でラップし、UndoStack<Document> で管理

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
| 5 | 2 | feat: implement CompNodeProcessor and TimeOffsetProcessor |
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
- `cargo test -p ravel-nodes` — CompNodeProcessor + TimeOffsetProcessor テスト
- `cargo test -p ravel-ui` — 既存テスト通過（Timeline 関連は更新）
- `RUSTFLAGS="-D warnings" cargo clippy` — clean
- `cargo fmt --check` — clean
- UI 手動テスト: Layer 追加/削除/並べ替え、in/out トリム、キーフレーム表示
- UI 手動テスト: PreComp（入れ子 Composition）動作確認
- **永続化テスト**: Composition の RON シリアライズ/デシリアライズ往復
- **premul alpha テスト**: Multiply/Screen/Overlay の合成結果がリファレンスと一致
- **solo/mute テスト**: solo 時に非 solo Layer が除外される
- **negative start_frame テスト**: Layer が Comp 先頭より前に配置された場合の動作
- **Undo テスト**: Graph + CompMap の統一スナップショットで undo/redo が正常動作

## リスク

- **AnimationChannel の統合**: Layer の Transform プロパティに AnimChannel を組み込む際、既存の Channel 実装と Layer のフレーム座標系（Comp ローカル vs ソースローカル）の整合が必要
- **コンパイラの再展開コスト**: Layer 追加/削除時にグラフ全体の再展開が走る。大規模 Composition ではパフォーマンス影響あり → 差分展開の最適化は後続タスク
- **既存デモグラフ**: NodeEditorPanel の `build_demo_graph` がCompNode を使ったデモに差し替えが必要
- **決定論的 ID の衝突**: CompId/LayerId のビットシフト方式で ID 空間が制限される。十分なビット幅の検証が必要
- **TimeOffset と EvalContext**: TimeOffset ノードが EvalContext.frame を変換する際、Evaluator のキャッシュキーが frame を含むため、変換後の frame で正しくキャッシュが機能するか検証が必要
