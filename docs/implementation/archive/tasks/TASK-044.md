# TASK-044: per-instance 変調（falloff 相当）
- **マイルストーン**: MS5 Motion Graphics
- **関連要件**: REQ-MOGRAPH-001 (v2), REQ-CORE-012
- **規模**: M
- **依存タスク**: TASK-040, TASK-043

## 概要
インスタンス属性（`index`/`P` 等）とフィールドを使って複製要素ごとの
パラメータ（位置オフセット/スケール/回転/色/時間オフセット）を変調する
仕組み。Cavalry の duplicator + falloff 相当。変調ノード（属性→transform
反映）と、時間オフセット属性による段差アニメーション（stagger）を含む。

## 実装ステップ
1. インスタンス変調ノード（field/式 → rot/scale/P オフセット/Cd 書き込み）
2. stagger: `delay = f(index or field(P))` を統一チャネル評価に接続
3. 代表プリセット動作の統合テスト（距離フォールオフでスケールが波打つ
   グリッド、index 段差のカスケードイン）
4. Properties パネルでの変調パラメータ表示

## 対象コンポーネント
- `crates/ravel-nodes/src/instance_modulate/`
- `crates/ravel-core/src/animation/`（時間オフセット適用点）
- `crates/ravel-app/src/panels/properties.rs`

## 完了条件
- [ ] フィールド/式でインスタンスごとの transform/色を変調できる
- [ ] index/フィールド由来の時間オフセット（stagger）が動作する
- [ ] 代表2ケースの統合テストが通る
