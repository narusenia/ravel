# TASK-046: テーブルデータ入力 + 属性バインディング
- **マイルストーン**: MS5 Motion Graphics
- **関連要件**: REQ-DATA-001, REQ-DATA-002
- **規模**: M
- **依存タスク**: TASK-038, TASK-043

## 概要
CSV/TSV/JSON を読み込む Table 入力ノード（型推論 + 明示指定、ファイル
ウォッチ再読み込み、Tokio I/O 分離）と、テーブル行→インスタンス生成・
列→属性バインディングを行うノードを実装する。

## 実装ステップ
1. `Table` 型（行×型付き列）の NodeData 統合
2. CSV/TSV/JSON パーサ（型推論、明示上書き、エラー行の報告）
3. ファイルウォッチ → dirty 伝播（Tokio、評価スレッド非ブロック）
4. Table→インスタンス生成ノード（行数分、列→属性マッピング UI 定義）
5. 既存ジオメトリへの属性付与モード
6. 統合テスト（データ更新反映、per-instance 変調との連携）

## 対象コンポーネント
- `crates/ravel-core/src/geometry/table.rs`
- `crates/ravel-nodes/src/data/`
- `crates/ravel-core/src/runtime/`（ウォッチャ）

## 完了条件
- [ ] CSV/TSV/JSON がテーブルとして読み込め、型指定できる
- [ ] ファイル更新で下流が dirty になり再評価される
- [ ] 行数連動のインスタンス生成と列→属性バインドが動作する
- [ ] 不正入力がエラー表示になりクラッシュしない
