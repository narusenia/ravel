# [CRIT-03] プロジェクト保存が非アトミック — クラッシュで .ravprj が破損する

| 項目 | 内容 |
| --- | --- |
| 深刻度 | critical |
| 種別 | bug |
| 領域 | ravel-app / 永続化 |
| 該当 | `crates/ravel-app/src/project/container.rs:173-188` |

> **解決済み**: フェーズ A2（保存は既存ファイルを `<path>.bak` へ退避したうえで書き、
> `ProjectFile::load_with_backup` が本体の検証に失敗したとき `.bak` を検証して
> 復帰する。復帰は `ProjectEvent::BackupRecovered` で通知される）。

## 現状

`write_file` は旧ファイルを `.bak` にコピーした後、`File::create` で対象を truncate して書き込む。
truncate と flush の間でクラッシュ・電源喪失が起きると `.ravprj` が破損したまま残る。

前リビジョンは `.bak` に残るが、`ProjectFile::load` は `.bak` へフォールバックしないし、
`.bak` の存在をユーザーに知らせる経路も無い。

## 影響

破損した瞬間、GUI からの復旧手段がゼロ。ユーザーはファイルマネージャで `.bak` を手動リネームする
必要があり、その存在を知る手段が無い。オートセーブもジャーナルも未配線
（[medium/app-shell.md](../medium/app-shell.md)）なので、これが唯一の防御線。

## 修正方針

1. 同一ディレクトリに一時ファイルを書き、`rename` で対象へ差し替える（POSIX/Windows 双方でアトミック）
2. 読み込み失敗時に `.bak` からの復旧を提示する

## 検証

- 書き込み途中を模した中断後、元ファイルが無傷であることを検証するテスト
- 破損ファイル読み込み時に `.bak` 復旧が提示されるテスト

## 関連

- [CRIT-02](CRIT-02-save-failure-invisible-and-swallows-quit.md)
