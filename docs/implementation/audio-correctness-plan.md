# 音声正確性計画

> **Status**: In progress — 2026-07-29

## 背景

フェーズ A3 は、再生キューの transport 境界、音声 seek の時間基準、
リサンプラの遅延、エンコーダのフレーム化、および出力デバイス設定を一つの
正確性境界として直す。

対象 issue は `HIGH-10`〜`HIGH-15`、`MED-MED-03`〜`MED-MED-05`、
`MED-AUD-01`。特に `HIGH-12`〜`HIGH-15` は、古いキュー、アンダーラン、
prep thread 上の重い処理が互いに A/V ドリフトを増幅するため、個別修正ではなく
epoch 付きキューへ置き換える。

## 目標アーキテクチャ

```text
transport command ──> epoch を更新 ──> prep thread が新 epoch の chunk を生成
                              └──────> callback は旧 epoch を破棄

SetTrack ──> resample worker ──> prepared track ──> prep thread の mixer

audio decode: sample position ──> AV_TIME_BASE seek ──> PTS 境界 trim
audio encode: interleaved input ──> pending frames ──> full codec frames
                                                   └─> finalize で末尾 flush

default output device ──> supported config ──> mixer / clock / stream の共通設定
```

リアルタイム callback は引き続き allocation、blocking、lock を行わない。
クロックは出力バッファの長さではなく、現 epoch のチャンクから実際にコピーした
フレーム数だけ進める。seek / pause と同時に走った callback は出力を無音化し、
seek 後のクロックへ旧 epoch の進行を加えない。

## A3-1: epoch 付き再生キュー

### 作業

- チャンクに epoch を付け、seek / pause で epoch を進める。
- callback は pause 中にキューを消費せず、旧 epoch の current / queued chunk を
  破棄する。
- prep thread の bounded send を command 割り込み可能にし、未再生の mix 位置を
  transport clock へ戻す。
- クロックをチャンク由来の出力フレーム数だけ進める。

### 完了条件

- Pause -> Resume と Seek の後に旧位置の音声が出ず、内容位置とクロックが一致する。
- アンダーランの無音でクロックが進まない。
- callback 経路に allocation、blocking、mutex が増えない。

## A3-2: SetTrack の非同期準備とリサンプラ終端

### 作業

- sample-rate 変換を専用 worker へ移し、prep thread は mixer と command 処理を
  継続できるようにする。
- track ごとの世代を付け、遅れて完了した旧 SetTrack が新しい編集を上書きしない。
- rubato の filter delay を出力先頭から除き、partial processing で末尾を回収して
  入力時間長に対応する出力フレーム数へ揃える。

### 完了条件

- 異なる sample rate の SetTrack が prep thread を同期的に塞がない。
- 同一 track の新しい SetTrack / RemoveTrack が古い worker 結果に負けない。
- impulse と既知長バッファのテストで先頭遅延と末尾欠落がない。

## A3-3: sample-accurate audio decode

### 作業

- `start_sample` から stream PTS と `AV_TIME_BASE` の microseconds を共に求め、
  container seek には後者を渡す。
- frame PTS を sample position へ変換し、目標より前の frame を捨て、境界 frame の
  先頭を sample 単位で trim する。

### 完了条件

- 非ゼロ位置の chunk が要求 sample から始まる。
- 上限ちょうどの full-audio decode probe が余分な sample を誤検出しない。

## A3-4: encoder の channel layout と固定 frame 化

### 作業

- 書き込み frame に `ChannelLayout::default_for_channels` を使う。
- 呼び出しをまたぐ pending interleaved buffer を持ち、固定 frame-size codec には
  完全 frame だけを送り、finalize で末尾を送る。
- 入力 chunk の rate / channel count が encoder 設定と違う場合は明示エラーにする。

### 完了条件

- 3 channel 以上の frame を正しい容量と layout で構築できる。
- frame-size 未満の複数 chunk が途中の短い frame を生成せず連結される。
- finalize が残りを一度だけ flush し、PTS が連続する。

## A3-5: 出力デバイス能力の採用

### 作業

- 既定 engine 設定では default output device の supported default config を採用する。
- 選択した sample rate / channel count を clock、mixer、AudioService の timeline
  frame 換算へ一貫して伝える。
- 明示設定はテストと将来の device selection 用に保持する。

### 完了条件

- 48 kHz stereo 非対応デバイスでも、そのデバイスの既定設定で起動を試みる。
- AudioService が実際の output rate で track placement を構築する。

## A3-6: 文書と完了ゲート

### 作業

- 対象 issue を解消済みにし、ロードマップ、バックログ、計画 index を同期する。
- この計画を `done/` へ移し、標準検証と `ravel-review` を通す。

### 完了条件

- `cargo test -p ravel-audio`
- `cargo test -p ravel-media --features ffmpeg`
- `cargo test -p ravel-app`
- `mise run check`
- PR 前の `ravel-review`
