# [CRIT-02] 保存失敗が不可視、かつガード付き保存の失敗で終了操作が黙殺される

| 項目 | 内容 |
| --- | --- |
| 深刻度 | critical |
| 種別 | bug |
| 領域 | ravel-app / 永続化・ワークスペース |
| 該当 | `crates/ravel-app/src/project_state.rs:443-447`, `crates/ravel-app/src/workspace.rs:911-919` |

> **解決済み**: フェーズ A2（`ProjectEvent::SaveFailed` / `SaveChangedDuringWrite` を
> emit し、`crates/ravel-app/src/workspace.rs` が workspace notification に落とす。
> 保存キューは FIFO で、完了コールバックが Quit / Close のガードを解く）。

## 現状

`spawn_save` は書き込みエラーを `SaveOutcome::Failed` に落とし、`tracing::error!` するだけ。
UI への通知経路が無い。ワークスペースルートには通知レイヤーが既に描画されているが、誰も使っていない。

さらに未保存確認ダイアログ経由（`queue_guarded_save`）では、`Failed` と `SavedButDirty` を
単に `return` で捨てる。ダイアログは既に閉じているため、保留していた Quit / Close / New / Open が
無言で破棄される。

## 影響

ディスク満杯・権限不足・書き込み不能パスで Cmd+S が失敗しても、ユーザーには一切表示されない。
「保存できたのにアプリが終了しないだけ」と誤認したまま、実際は保存されておらず作業が失われる。
データ損失に直結する。

## 修正方針

1. 保存失敗をダイアログまたは通知でローカライズ表示する
2. ガード付き保存の失敗時は保留アクションを破棄せず、再プロンプトする
3. `SavedButDirty`（保存中に更に変更）も同様に扱う

## 検証

- 書き込み不能パスに対する保存でエラー表示が出るテスト
- ガード付き保存失敗後に Quit が再確認されるテスト

## 関連

- [CRIT-03](CRIT-03-project-write-not-atomic.md) — 保存そのものの破損リスク
- [HIGH-18](HIGH-18-open-failure-invisible.md) — Open 側の同種問題
- [medium/app-shell.md](../medium/app-shell.md) — オートセーブ・ジャーナル未配線
