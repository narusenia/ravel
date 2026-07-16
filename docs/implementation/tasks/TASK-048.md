# TASK-048: コード Layer / コードノード基盤
- **マイルストーン**: MS6 Pro Features
- **関連要件**: REQ-CODE-001
- **規模**: L
- **依存タスク**: TASK-032, TASK-038, TASK-040

## 概要
Lua スクリプトを Layer ソース/ノードとして実行する基盤。時間純関数契約
（`render(ctx) -> Geometry | FrameBuffer`）、`params { ... }` の型付き
パラメータ宣言と Properties/統一チャネル統合、Lua からのジオメトリ属性
API・フィールドサンプル呼び出し、Layer 単位のエラー分離を実装する。

## 実装ステップ
1. コードノードの NodeData/processor 実装（mlua 実行、出力型判定）
2. `params` 宣言のパース → パラメータテーブル生成 → Properties 表示
3. パラメータ値の統一チャネル接続（キーフレーム/式/フィールド）
4. Lua ジオメトリ API（属性読み書き、フィールドサンプル、シェイプ生成）
5. Layer ソース `Code` の composition コンパイラ対応
6. エラー分離（スクリプト例外 → Layer エラー表示、評価継続）
7. サンドボックス検証（io/os 遮断が REQ-PLUGIN-003 と同一であること）
8. サンプルスクリプト + 統合テスト

## 対象コンポーネント
- `crates/ravel-nodes/src/code/`（新設）
- `crates/ravel-core/src/composition/compile.rs`
- `crates/ravel-app/src/panels/properties.rs`

## 完了条件
- [ ] Lua コード Layer が時間関数として評価され Geometry/FrameBuffer を出力する
- [ ] 宣言パラメータがキーフレーム・式・フィールド接続可能
- [ ] スクリプトエラーが Layer 単位に分離される
- [ ] サンドボックス制約が既存 Lua 式と同一
