# [HIGH-18] File ▸ Open の失敗が不可視

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-app / 永続化 |
| 該当 | `crates/ravel-app/src/project_state.rs:495-497` |

## 現状

`load_project_from` のエラーアームはログ出力のみ。

破損アーカイブ、将来バージョンで作られたプロジェクト（`MigrationError::TooNew`）、
読み取り不能ファイルを開いても何も表示されない。
ユーザーにはメニュー操作が無視されたように見える。
`ProjectError` / `MigrationError::TooNew` は良質なメッセージを持つが、UI に届かない。

## 影響

「開けない理由が分からない」状態。特に `TooNew` は新しい Ravel で作ったファイルという
明確な原因があるのに伝わらない。

## 修正方針

読み込みエラーをダイアログ / 通知へルーティング。
`TooNew` は「より新しいバージョンの Ravel で作成されたプロジェクト」と特別扱いする。
文言は `t!` でローカライズ。

## 検証

- 破損ファイル・`TooNew` ファイルでエラー表示が出るテスト

## 関連

- [CRIT-02](../critical/CRIT-02-save-failure-invisible-and-swallows-quit.md), [HIGH-20](HIGH-20-media-import-failure-invisible.md) — 同種のエラー不可視問題。共通の通知経路を作るべき
