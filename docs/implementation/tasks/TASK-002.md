# TASK-002: DAG評価エンジン (Hybrid Pull + Dirty Notification)
- **マイルストーン**: MS1 Foundation
- **関連要件**: REQ-CORE-001, REQ-CORE-002
- **規模**: L
- **依存タスク**: TASK-001

## 概要

ノードグラフの評価エンジンを実装する。出力ノードからPull型で再帰的にデータを要求し、上流パラメータ変更時にはdirtyフラグをPush的に下流へ伝播させることで不要な再評価を回避するHybridモデル。DAG上の全処理の実行基盤となる。

## 実装ステップ

1. **EvalContextの定義**
   - `EvalContext`構造体: 現在フレーム(`frame: u64`)、時間(`time: f64`)、FPS(`fps: FrameRate`)、キャッシュ参照、dirtyセット参照
   - フレーム↔時間の変換ユーティリティ
   - ノード評価結果の型: `Arc<dyn NodeData>`

2. **ノード処理トレイトの定義**
   - `NodeProcessor`トレイト: `fn process(&self, inputs: &[Arc<dyn NodeData>], ctx: &EvalContext) -> Result<Arc<dyn NodeData>>`
   - ノードタイプレジストリ（`NodeTypeKey` → `Box<dyn NodeProcessor>`のマップ）
   - パススルーノード、定数ノード等の基本実装

3. **Pull型評価器の実装**
   - 出力ノードから`evaluate(node_id, frame, ctx)`を再帰呼び出し
   - 入力ポートの接続先を辿り、上流ノードを先に評価
   - 評価結果をキャッシュに格納（ノードID + フレーム番号がキー）
   - キャッシュヒット時はキャッシュから即時返却

4. **Dirty flag伝播の実装**
   - パラメータ変更時に当該ノードをdirty化
   - DAGのエッジを辿り下流ノード全てにdirtyフラグを伝播（BFS）
   - dirty化されたノードのキャッシュを無効化
   - 出力に接続されていないノードはdirty化しても評価対象外

5. **トポロジカル評価順序の実装**
   - TASK-001のトポロジカルソート結果を利用
   - 並列評価可能なノード群の特定（同一レベルのノード → rayonで並列化の準備）
   - 評価順序の事前計算とキャッシュ

6. **ユニットテストの実装**
   - 線形チェーン（A→B→C）の評価順序と結果
   - ダイヤモンド依存（A→B→D, A→C→D）で重複評価が発生しないこと
   - 循環検出 → エラー返却
   - dirty伝播: B変更時にC,Dがdirty化、Aは非dirty
   - 部分評価: 出力に未接続のノードが評価されないこと
   - キャッシュヒット: 同一フレーム再評価でprocessが呼ばれないこと

## 対象コンポーネント

- `crates/ravel-core/src/eval/mod.rs` — 評価エンジンメインモジュール
- `crates/ravel-core/src/eval/context.rs` — EvalContext定義
- `crates/ravel-core/src/eval/evaluator.rs` — Pull型評価器
- `crates/ravel-core/src/eval/dirty.rs` — Dirty flag伝播
- `crates/ravel-core/src/eval/processor.rs` — NodeProcessorトレイト + レジストリ
- `crates/ravel-core/src/eval/cache.rs` — 評価結果キャッシュ（RAMレベル、TASK-019で三層化）

## 完了条件

- [ ] `EvalContext`が定義され、フレーム/時間/FPS情報を保持する
- [ ] `NodeProcessor`トレイトが定義され、ノードタイプレジストリが動作する
- [ ] 出力ノードからPull型で再帰評価が実行される
- [ ] パラメータ変更時にdirtyフラグが下流ノードに伝播する
- [ ] dirtyでないノードはキャッシュから結果を返す
- [ ] 出力に未接続のノードは評価されない
- [ ] 線形チェーンの評価テストが通る
- [ ] ダイヤモンド依存で重複評価しないテストが通る
- [ ] 循環検出テストが通る
- [ ] dirty伝播のテストが通る
- [ ] キャッシュヒットのテストが通る
