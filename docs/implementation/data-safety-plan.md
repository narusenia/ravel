# データ保全・失敗可視化計画

> **Status**: In progress — 2026-07-29

## 背景

フェーズ A2 は、保存中断によるプロジェクト破損、保存・読み込み・メディア
インポート失敗の不可視化、Viewer の未コミットジェスチャー混入、および
入力値や深いグラフによる latent crash をまとめて解消する。

対象 issue は `CRIT-02`〜`CRIT-04`、`HIGH-18`、`HIGH-20`、
`MED-APP-12`、`MED-CORE-04`、`MED-GPU-03`、`LOW-APP-01`、
`LOW-GPU-01`。保存と通知は同じ非同期完了経路を触り、深さ制限は
ロード境界と評価器の両方へ入るため、局所 issue の集合ではなく 1 つの
横断計画として扱う。

## 目標アーキテクチャ

```text
Project save
  archive bytes -> same-directory temporary file -> flush/sync -> atomic replace
                                                        └-> previous revision: .bak

Project operations / media import / GPU startup
  typed ProjectEvent -> workspace subscription -> localized error notification

Viewer gesture
  begin snapshot -> live apply* -> commit on accept
                              └-> restore begin snapshot on cancel

Untrusted graph / numeric input
  load-time subnet depth validation + eval recursion budget + GPU parameter clamp
```

`ravel-core` は UI を知らず、`ravel-app` の `ProjectState` が型付きイベントを
発行する。`RavelWorkspace` は実ウィンドウ上でイベントを通知へ変換する。
一回限りの通知に Global は使わない。

## A2-1: アトミック保存とバックアップ復旧

### 作業

- 保存先と同じディレクトリの一時ファイルへ全バイトを書き、`sync_all` 後に
  置換する。公開前の失敗では既存 `.ravprj` を変更しない。
- 既存ファイルは置換前に `.bak` へコピーし、直前の正常リビジョンを残す。
- primary の読み込みが失敗した場合は `.bak` を検証して復旧する。ただし
  `TooNew` は古いバックアップで黙って巻き戻さず、そのまま互換性エラーにする。
- primary と backup の両方が壊れている場合は両方の原因を保持する。

### 完了条件

- 2 回目の保存がアトミック置換され、`.bak` が前リビジョンを保持する。
- 公開前に失敗しても既存ファイルが無傷である。
- 破損 primary から正常 `.bak` を読み、復旧した事実を呼び出し側へ返す。
- `TooNew` では fallback しない。

## A2-2: 失敗通知と destructive action の再提示

### 作業

- `ProjectState` に保存失敗、Open 失敗、backup 復旧、インポート skip を表す
  型付き `EventEmitter` を追加する。
- Workspace がイベントを購読し、`t!` で英日ローカライズした error/warning
  notification を表示する。
- guarded save が `Failed` または `SavedButDirty` になった場合、保留 action を
  実行せず unsaved-changes dialog を再提示する。
- GPU 初期化失敗は panic せず、起動後に永続 error notification を表示する。

### 完了条件

- 保存不能、破損/TooNew Open、skip を含む import がそれぞれ通知イベントを出す。
- guarded save 失敗後も文書は dirty のままで、Quit / Close / New / Open の確認が
  再提示される。
- GPU adapter が無い場合にプロセスが panic しない。

## A2-3: Viewer ジェスチャー隔離と Duplicate

### 作業

- pen、move、shape、path-edit の開始時に Document snapshot を保持する。
- Escape / lost-button cancel は `DocumentStore` の現在の dirty 状態ではなく、
  その gesture snapshot を復元する。
- snapshot 復元は undo 履歴へ新しい step を加えず、以後の編集を dirty として
  正しく扱える `DocumentStore` API に集約する。
- Node Editor の Duplicate は保存済み clipboard を変更せず、一時 content を
  直接複製する。

### 完了条件

- gesture 中に別パネルが commit しても Escape が開始時 snapshot を復元する。
- 通常の gesture は従来どおり 1 undo step になる。
- Copy A -> Duplicate B -> Paste が A を貼り付ける。

## A2-4: core / GPU の入力防御

### 作業

- Document の subnet nesting に定数化した上限を設け、ロード後の正規化より前と
  `Document::validate` の先頭で反復走査により検証する。
- evaluator の node pull に独立した再帰 budget を渡し、上限超過を
  `EvalError` として返す。
- blur radius は registry の hard range と GPU dispatch の両方で安全な最大値へ
  clamp し、NaN / infinity も有限値へ正規化する。
- texture readback の容量は `u64` で計算して `usize` へ検査付き変換する。

### 完了条件

- 深い subnet 文書は stack overflow せず validation error になる。
- 深い直線 graph は stack overflow せず `EvalError` になる。
- 巨大・非有限 blur radius が shader loop 上限を超えない。
- 16384 x 16384 RGBA32F の容量計算が `2^32` を正しく保持する。

## 検証

- 各単位の headless / GPUI 回帰テスト
- `cargo test -p ravel-core`
- `cargo test -p ravel-gpu`
- `cargo test -p ravel-nodes`
- `cargo test -p ravel-app`
- `mise run check`
- PR 前の `ravel-review`
