# TASK-004: アンドゥシステム (イミュータブル + 構造共有)
- **マイルストーン**: MS1 Foundation
- **関連要件**: REQ-CORE-004, REQ-PROJ-002
- **規模**: M
- **依存タスク**: TASK-001

## 概要

`im`クレートのpersistent data structureを活用し、構造共有ベースのアンドゥ/リドゥシステムを実装する。グラフ変更時は変更部分だけ新規作成し未変更部分は`Arc`で共有することでメモリ効率を確保。併せて操作ジャーナル（WAL的）を実装し、クラッシュリカバリの基盤とする。

## 実装ステップ

1. **`im`クレートの導入**
   - `im` crateを`ravel-core`の依存に追加
   - TASK-001で定義した`Graph`構造体を`im::HashMap`ベースに確定
   - `GraphVersion`構造体: `im::HashMap<NodeId, Arc<Node>>` + `im::HashMap<EdgeId, Edge>`

2. **GraphVersionの実装**
   - イミュータブルなグラフスナップショット
   - ノード追加/削除/更新時に新バージョンを生成
   - 構造共有: 変更されたエントリだけ新しい`Arc<Node>`を作成、残りは既存`Arc`を共有
   - `GraphVersion::apply_mutation(&self, mutation: GraphMutation) -> GraphVersion`

3. **UndoStackの実装**
   - `versions: Vec<Arc<GraphVersion>>` + `current: usize`
   - `push(new_version)`: current以降を切り捨て、新バージョンを追加
   - `undo()`: currentデクリメント → 前バージョンを返却
   - `redo()`: currentインクリメント → 次バージョンを返却
   - `can_undo() -> bool`, `can_redo() -> bool`
   - オプション: 最大履歴数制限（メモリ圧迫防止）

4. **操作ジャーナル（append-only log）の実装**
   - `GraphMutation` enum: `AddNode`, `RemoveNode`, `UpdateParameter`, `AddEdge`, `RemoveEdge`等
   - ジャーナルファイル（`.journal/`内）にシリアライズして追記
   - シリアライズ形式: bincode（高速）またはRON（デバッグ容易）
   - ジャーナルエントリにタイムスタンプ + 操作シーケンス番号を付与

5. **ジャーナルリプレイによるクラッシュリカバリ**
   - 起動時に最後の保存済みグラフ + ジャーナルを検出
   - ジャーナルエントリを順次リプレイし最新状態を復元
   - リプレイ失敗時のエラーハンドリング（破損エントリのスキップ + 警告）

6. **ジャーナルコンパクション（正常終了時）**
   - 正常終了時: グラフを保存 → ジャーナルファイル削除
   - コンパクション中の中断対策（先に保存、後にジャーナル削除）
   - コンパクション後の確認（保存データの整合性チェック）

## 対象コンポーネント

- `crates/ravel-core/src/undo/mod.rs` — アンドゥシステムメインモジュール
- `crates/ravel-core/src/undo/stack.rs` — UndoStack
- `crates/ravel-core/src/undo/journal.rs` — 操作ジャーナル（BincodeCodec / RonCodec）
- `crates/ravel-core/src/undo/mutation.rs` — GraphMutation enum定義
- `crates/ravel-core/src/undo/recovery.rs` — クラッシュリカバリ

> **実装メモ**: 仕様書の`GraphVersion`は独立struct化せず、既存`Graph`が`im::HashMap`で構造共有を
> 実現しているためそのまま`UndoStack`に`Graph`を格納。版ごとのメモリコストは変更差分のみ。
> ジャーナルcodecはトレイト`JournalCodec`で抽象化し、`BincodeCodec`（デフォルト）/`RonCodec`（デバッグ用）を提供。
> 18テスト（stack 8, journal 5, recovery 3, codec 2）で検証。

## 完了条件

- [x] `im::HashMap`ベースの`GraphVersion`が定義されている（`Graph`自体が`im::HashMap`で構造共有）
- [x] グラフ変更時に新バージョンが生成され、未変更部分はArc共有される（メモリ測定テスト）
- [x] UndoStackのundo/redoが正しく動作する（ユニットテスト）
- [x] undo後に新操作を行うとredo履歴が破棄される（ユニットテスト）
- [x] 操作ジャーナルがファイルに追記される
- [x] ジャーナルリプレイでグラフ状態が復元される（インテグレーションテスト）
- [x] 正常終了時にジャーナルがコンパクションされる
- [x] 破損ジャーナルエントリ存在時にスキップして復元できる（エラーハンドリングテスト）
