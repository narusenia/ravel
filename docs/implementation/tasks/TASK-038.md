# TASK-038: ジオメトリコンテナ + 属性システム
- **マイルストーン**: MS5 Motion Graphics（前提基盤）
- **関連要件**: REQ-CORE-010
- **規模**: L
- **依存タスク**: なし

## 概要
`ravel-core::geometry` モジュールを新設し、列指向の `Geometry` コンテナ
（points / primitives / instances / detail の4ドメイン）と型付き属性列
（`AttributeArray`）、属性セット（`AttributeSet`、`Arc` による構造共有 +
コピーオンライト）を実装する。標準属性名（`P`/`index`/`id`/`rot`/`scale`/
`Cd` 等）を定数として予約し、`NodeData`/`GeometricData` を実装してノード間を
流せるようにする。詳細は `docs/specifications/procedural-geometry.md`。

## 実装ステップ
1. `AttributeArray`（F32/Vec2/Vec3/Vec4/Color/I32/Bool/Str 列）と型検証
2. `AttributeSet`（名前→`Arc<AttributeArray>`、CoW 変更 API）
3. `Geometry` コンテナ（4ドメイン、要素数整合の構築時検証）
4. 標準属性名の予約定数と生成ヘルパ
5. `NodeData` + `GeometricData` 実装、ポート型判定への統合
6. プロパティ/デバッグ表示用の要約（要素数・属性一覧）API
7. ユニットテスト（CoW 共有、ドメイン整合、型変換エラー）

## 対象コンポーネント
- `crates/ravel-core/src/geometry/`（新設: mod.rs, attribute.rs, container.rs）
- `crates/ravel-core/src/types.rs`（GeometricData 統合）

## 完了条件
- [x] 任意名・任意型の属性を point / instance ドメインに付与できる
- [x] `Arc` 構造共有と CoW 変更が undo（イミュータブルグラフ）と両立する
- [x] ドメイン内の属性列長不一致が構築時エラーになる
- [x] `Geometry` がノード間を流れ、ポート型互換判定に参加する
- [x] ユニットテストが headless で通る
