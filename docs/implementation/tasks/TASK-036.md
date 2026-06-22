# TASK-036: 自動アップデーター
- **マイルストーン**: MS7 Polish
- **関連要件**: REQ-INFRA-003
- **規模**: M
- **依存タスク**: TASK-006

## 概要
起動時のアップデートチェック（HTTPS）、Stable/Beta/Nightlyチャネル選択、macOSでのSparkleフレームワーク統合、WindowsでのWinSparkleまたはカスタムHTTPアップデーター、アップデート通知UI、Nightlyビルド用CIパイプライン、プラグインAPI互換性のためのsemverバージョニングルール定義を実装する。

## 実装ステップ
1. 起動時アップデートチェック実装（HTTPS）
2. Stable/Beta/Nightlyチャネル選択実装
3. macOS: Sparkleフレームワーク統合
4. Windows: WinSparkleまたはカスタムHTTPアップデーター実装
5. アップデート通知UI実装
6. Nightlyビルド用CIパイプライン実装
7. プラグインAPI互換性のためのsemverバージョニングルール定義

## 対象コンポーネント
- `crates/ravel-updater/` (アップデーターエンジン)
- `crates/ravel-updater/src/channel/` (チャネル管理)
- `crates/ravel-updater/src/platform/` (プラットフォーム固有実装)
- `crates/ravel-ui/src/panels/update_notification/` (アップデート通知UI)
- `.github/workflows/` (CIパイプライン)

## 完了条件
- [ ] 起動時にHTTPSでアップデートチェックが実行される
- [ ] Stable/Beta/Nightlyのチャネル選択が動作する
- [ ] macOSでSparkleフレームワーク経由のアップデートが動作する
- [ ] Windowsでアップデートが動作する
- [ ] アップデート通知UIが表示される
- [ ] Nightlyビルド用CIパイプラインが動作する
- [ ] semverバージョニングルールが定義済み
