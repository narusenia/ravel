# [HIGH-20] メディアインポートの失敗が不可視

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-app / メディアインポート |
| 該当 | `crates/ravel-app/src/media/import.rs:143-149`, `crates/ravel-app/src/project_state.rs:576-694` |

## 現状

プローブ失敗は `ImportSummary.skipped` に入るが、これはログ出力のみで UI に届かない。

## 影響

10ファイルをドロップして7レイヤーだけできる — 3つが無言で消える。
ffmpeg フィーチャ無効ビルドではコンテナのインポートがすべて不可視に失敗する。

## 修正方針

`skipped`（件数 + 理由）をアラートダイアログまたは通知で表示。文言は `t!` でローカライズ。

## 検証

- 未対応ファイルを含むドロップでスキップ件数が表示されるテスト

## 関連

- [CRIT-02](../critical/CRIT-02-save-failure-invisible-and-swallows-quit.md), [HIGH-18](HIGH-18-open-failure-invisible.md) — 共通のユーザー通知経路として一括で整備すべき
