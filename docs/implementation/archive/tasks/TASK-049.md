# TASK-049: シーケンス糖衣 API（timeline ビルダ）
- **マイルストーン**: MS6 Pro Features
- **関連要件**: REQ-CODE-002
- **規模**: M
- **依存タスク**: TASK-048

## 概要
manim / motion-canvas 的な逐次記述（tween / wait / 並列 / stagger）を宣言的
に構築する Lua 標準ライブラリ。ビルダは時間純関数へコンパイルし、逐次実行や
ベイクを行わない。イージングはアニメーションカーブ実装を共有する。

## 実装ステップ
1. timeline ビルダ DSL（tween/wait/parallel/stagger/ease 指定）
2. ビルダ → 区間関数列への正規化（開始時刻解決、重なり合成規則）
3. イージング関数群のバインド（animation/interpolation を共有）
4. 代表サンプル（テキストカスケード、図形モーフ相当）の統合テスト
5. スクラブ・巻き戻し・尺変更の回帰テスト

## 対象コンポーネント
- `crates/ravel-nodes/src/code/timeline.rs`
- `assets/`（サンプルスクリプト同梱を検討）

## 完了条件
- [ ] tween/wait/parallel/stagger が宣言でき期待通り再生される
- [ ] スクラブ・巻き戻しで破綻しない（純関数性の担保）
- [ ] 糖衣なしの素の時間関数と混在できる
