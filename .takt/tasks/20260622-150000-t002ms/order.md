# タスク仕様

## 目的

Hybrid Pull + Dirty Notification方式のDAG評価エンジンを実装する。出力ノードからの再帰的プル評価と、変更時のダーティフラグ伝播により効率的なグラフ再計算を実現する。

## 要件

- [ ] Pull-based評価器の実装（出力ノードからの再帰的評価）
- [ ] Dirtyフラグ伝播メカニズム（ノード変更時に下流へ通知）
- [ ] トポロジカル順序に基づく評価スケジューリング
- [ ] EvalContextの定義（frame, time, fps, resolution等）
- [ ] Nodeプロセッシングトレイト（`fn process(&self, ctx: &EvalContext, inputs: &[&dyn NodeData]) -> Result<Box<dyn NodeData>>）
- [ ] ユニットテスト：ダイヤモンド依存の重複評価防止
- [ ] ユニットテスト：循環依存の検出とエラーハンドリング
- [ ] ユニットテスト：Dirtyフラグ伝播の正確性検証

## 受け入れ基準

- ダイヤモンド依存で共有ノードが1回だけ評価される
- 循環参照グラフ投入時にパニックせずエラー返却
- Dirtyでないノードはスキップされ再評価されない
- EvalContext変更（フレーム進行）で時間依存ノードのみ再評価
- 100ノード規模のグラフで評価完了

## 参考情報

- docs/specifications/architecture.md
- REQ-CORE-001（ノードグラフモデル）
- REQ-CORE-002（DAG評価エンジン）
- 依存: TASK-001（型システム + ノードグラフデータモデル）
