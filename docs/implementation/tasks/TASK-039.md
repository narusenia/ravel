# TASK-039: 属性操作ノード群
- **マイルストーン**: MS5 Motion Graphics（前提基盤）
- **関連要件**: REQ-CORE-010
- **規模**: M
- **依存タスク**: TASK-038

## 概要
属性の生成・転送・昇格・サンプリングを行う汎用ノード群を実装する。
Attribute Set（式/定数で属性書き込み）、Attribute Transfer（近傍転送）、
Attribute Promote（ドメイン間変換）、Path Sample（パス上の位置・接線取得）、
Lua からの属性参照（`@attr` 相当のバインディング）を含む。

## 実装ステップ
1. Attribute Set ノード（定数/Lua 式で任意属性を書き込み）
2. Attribute Promote（point↔instance↔detail、平均/最大/最初の集約規則）
3. Attribute Transfer（最近傍/距離重み補間）
4. Path Sample（弧長パラメータで P/接線/法線を返す）
5. mlua バインディング: 式スコープに属性リーダを注入
6. 各ノードの registry 登録 + processor 実装
7. ユニットテスト（転送精度、昇格集約、式アクセス）

## 対象コンポーネント
- `crates/ravel-core/src/geometry/ops.rs`（純ロジック）
- `crates/ravel-nodes/src/attribute/`（processor 群）
- `crates/ravel-core/src/registry/builtin.rs`

## 完了条件
- [ ] 属性の追加・削除・上書きがノードで行える
- [ ] 転送・昇格・サンプリングが仕様どおり動作する
- [ ] Lua 式から属性値を参照できる
- [ ] 属性が複製・変形ノードを通じて伝播することを統合テストで確認
