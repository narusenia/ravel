# TASK-047: 属性スプレッドシートパネル
- **マイルストーン**: MS5 Motion Graphics
- **関連要件**: REQ-CORE-010（検査性）, REQ-UI-004
- **規模**: M
- **依存タスク**: TASK-038

## 概要
選択ノードの出力ジオメトリの属性を表形式で検査するパネル（Houdini の
Geometry Spreadsheet 相当）。ドメイン切替（point/primitive/instance/detail）、
属性列の表示、大規模データのページング表示を提供する。プロシージャル
ワークフローのデバッグ性の要。

## 実装ステップ
1. PanelKind への追加（ワークスペースプリセット/トグル対応）
2. 選択ノード出力の Geometry 要約取得（評価結果キャッシュから）
3. ドメイン切替タブ + 仮想スクロール表
4. 属性値のフォーマット表示（Vec2/Color 等）
5. Composition/Layer 選択との連動（focused node 追従）

## 対象コンポーネント
- `crates/ravel-ui/src/panel.rs`（PanelKind 追加）
- `crates/ravel-app/src/panels/attribute_sheet.rs`（新設）
- `assets/locales/`（ラベル追加）

## 完了条件
- [ ] 選択ノードの属性がドメイン別に表示される
- [ ] 10万要素でもスクロールが破綻しない（仮想化）
- [ ] パネルトグル・プリセット・focus 規約（gpui.md）に準拠する
