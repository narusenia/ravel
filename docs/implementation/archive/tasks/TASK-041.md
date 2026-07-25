# TASK-041: ステートフル評価 + シミュレーションキャッシュ
- **マイルストーン**: MS5 Motion Graphics（前提基盤）
- **関連要件**: REQ-CORE-011
- **規模**: L
- **依存タスク**: TASK-038

## 概要
`StatefulProcessor` trait（`initial` / `step` の純関数ペア）を導入し、
Evaluator にフレーム逐次評価と sim キャッシュ（フレーム連続区間の
`Vec<Arc<State>>` + 上流ハッシュ）を実装する。未計算区間の充填、上流変更
時の破棄、後方スクラブのキャッシュ供給、前方ジャンプ時の暫定表示 +
バックグラウンド充填を含む。詳細は
`docs/specifications/procedural-geometry.md` の「ステートフル評価」。

## 実装ステップ
1. `StatefulProcessor` trait と Evaluator への登録経路
2. sim キャッシュ構造（区間 + 上流構造/パラメータハッシュ）
3. Pull 要求 → 未計算区間 `[last+1, t]` の逐次 step 充填
4. 上流 dirty 時のキャッシュ破棄（v1: 全区間）
5. 評価スレッドプールでの区間充填（UI 非ブロック、暫定フレーム表示）
6. 決定性テスト（同一 seed・同一入力で bit-exact）
7. ステートレス経路の性能非退行ベンチ（criterion）

## 対象コンポーネント
- `crates/ravel-core/src/eval.rs`
- `crates/ravel-core/src/geometry/sim.rs`（新設）

## 完了条件
- [ ] ステートフルノードが前フレーム状態を参照して評価される
- [ ] 後方スクラブが再計算なしで表示される
- [ ] 上流変更でキャッシュが破棄・再充填される
- [ ] UI スレッドがブロックされない
- [ ] 決定性がテストで担保される
- [ ] ステートレス評価のベンチに退行がない
