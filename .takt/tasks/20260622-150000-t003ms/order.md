# タスク仕様

## 目的

マルチスレッド評価基盤とCI/CDパイプラインを構築する。rayon・Tokio・専用デコードプールによるスレッディングモデルと、GitHub Actionsによる自動テスト・品質チェック環境を整備する。

## 要件

- [ ] rayonスレッドプールによるグラフ並列評価の統合
- [ ] 専用メディアデコードプール（std::thread固定スレッド）
- [ ] TokioランタイムによるI/O非同期処理基盤
- [ ] crossbeam-channelによるスレッド間メッセージパッシング
- [ ] tracing crateによる構造化ログ基盤（span/event）
- [ ] GitHub Actions CI設定（cargo test, cargo clippy, cargo fmt --check）
- [ ] macOS + Windowsのクロスプラットフォームビルド検証
- [ ] criterionベンチマークスケルトン（グラフ評価スループット計測用）

## 受け入れ基準

- グラフ評価がrayonで並列実行され、シングルスレッド比で高速化確認
- デコードプールがメインスレッドをブロックしない
- tracing出力でスレッドID・スパン情報が正しく記録される
- GitHub Actions CI（macOS + Windows）が全テスト・clippy・fmtで通過
- criterionベンチマークが `cargo bench` で実行可能

## 参考情報

- docs/specifications/architecture.md
- REQ-CORE-005（スレッディングモデル）
- 依存: TASK-001（型システム + ノードグラフデータモデル）
