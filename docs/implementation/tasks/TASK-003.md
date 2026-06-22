# TASK-003: スレッディングモデル + CI基盤
- **マイルストーン**: MS1 Foundation
- **関連要件**: REQ-CORE-005, REQ-INFRA-001, REQ-INFRA-004, REQ-INFRA-006
- **規模**: M
- **依存タスク**: TASK-001

## 概要

Ravelの並行処理基盤を構築する。用途別にスレッドプールを分離し、ロックフリーなメッセージパッシングで連携させる。併せてCI/CDパイプラインとログ基盤を整備し、継続的な品質維持を可能にする。

## 実装ステップ

1. **rayon評価スレッドプールの構成**
   - `rayon::ThreadPoolBuilder`でカスタムスレッドプール作成
   - スレッド数はCPUコア数ベースで自動調整（設定でオーバーライド可）
   - ノードグラフ評価（TASK-002）の並列実行用

2. **デコードスレッドプールの構成**
   - FFmpegデコード専用の固定サイズスレッドプール（`std::thread`ベース）
   - デコードジョブキュー（`crossbeam-channel`の`bounded`チャネル）
   - バックプレッシャー制御（キュー満杯時のブロッキング）

3. **Tokioランタイムの構成**
   - `tokio::runtime::Builder`でマルチスレッドランタイム作成
   - ファイルI/O、ネットワーク、プラグインホスト制御に使用
   - ランタイムを`static`に保持し各所からアクセス可能にする

4. **crossbeam-channelによるスレッド間メッセージパッシング**
   - UIスレッド → 評価プール: `EvalRequest`（フレーム評価要求）
   - 評価プール → UIスレッド: `EvalResult`（評価結果）
   - デコードプール → 評価プール: `DecodeResult`（デコード済みフレーム）
   - メッセージ型をenumで定義し型安全性を確保

5. **tracing/ログ基盤の整備**
   - `tracing` + `tracing-subscriber`でログ出力
   - ファイル出力（`tracing-appender`でローテーション）
   - 構造化ログ（JSON形式）
   - ログレベル設定（環境変数`RAVEL_LOG`で制御）
   - 各スレッドプールにスパン設定

6. **GitHub Actions CI構成**
   - `.github/workflows/ci.yml`作成
   - macOS + Windowsの両環境でビルド・テスト
   - `cargo test` → `cargo clippy -- -D warnings` → `cargo fmt --check`
   - キャッシュ設定（`actions/cache`でtargetディレクトリ）
   - PR + pushトリガー

7. **criterionベンチマークスケルトン**
   - `benches/`ディレクトリ構成
   - トポロジカルソート、グラフ評価のベンチマーク骨格
   - CI上でベンチマーク実行（結果記録、回帰検出は将来）

## 対象コンポーネント

- `crates/ravel-core/src/runtime/mod.rs` — ランタイム管理
- `crates/ravel-core/src/runtime/eval_pool.rs` — rayon評価プール
- `crates/ravel-core/src/runtime/decode_pool.rs` — デコードスレッドプール
- `crates/ravel-core/src/runtime/io_runtime.rs` — Tokioランタイム
- `crates/ravel-core/src/runtime/channels.rs` — メッセージ型定義 + チャネル構成
- `crates/ravel-core/src/logging.rs` — tracing設定
- `.github/workflows/ci.yml` — GitHub Actions CI
- `crates/ravel-core/benches/` — criterionベンチマーク

## 完了条件

- [ ] rayon評価スレッドプールが構成され、並列タスク実行が動作する
- [ ] デコードスレッドプールが構成され、ジョブキュー経由でタスク投入可能
- [ ] Tokioランタイムが起動しasync I/Oが実行可能
- [ ] crossbeam-channelでスレッド間メッセージ送受信が動作する
- [ ] `tracing`による構造化ログがファイル出力される
- [ ] GitHub Actions CIでmacOS + Windowsの`cargo test`が通る
- [ ] GitHub Actions CIでclippy + fmt checkが通る
- [ ] criterionベンチマークスケルトンが`cargo bench`で実行可能
