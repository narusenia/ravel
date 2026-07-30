# 音声編集追従計画

> **Status**: Done (#212) — 2026-07-30

## 背景

フェーズ A4 は、音声レイヤーを配置して再生する最小操作に残る準備待ち、
編集反映の遅延、終端停止の取りこぼし、および無言の失敗を一つの利用可能性境界として
直す。

対象 issue は `HIGH-23`、`HIGH-24`、`MED-AUD-02`、`MED-AUD-03`。
現在は source rate の全長バッファを `SetTrack` のたびに engine 側で変換するため、
配置や mute の変更まで古い全長変換を再実行する。また、Composition 終端で frame が
変わらない tick は pause 状態を公開せず、音声 engine だけが再生を続ける。

## 目標アーキテクチャ

```text
asset + stream ──> background decode ──> output-rate SRC ──> AudioService cache
                                                        │
document edit ──> TrackSpec diff ──> slice / placement ─┴─> SetTrack ─> mixer

AudioService state ──> Timeline / MediaBin の「準備中」表示
                  └──> decode / SRC 失敗 event ──> workspace notification

wall/audio clock ──> Transport tick
                       ├─ frame changed
                       └─ Playing → Paused ──> transport update ──> engine Pause
```

リアルタイム callback と prep thread は引き続きデコードや sample-rate 変換を行わない。
変換済みバッファは asset id と stream index ごとに共有し、レイヤーの配置、trim、
mute、solo、fade の編集はそのバッファを再利用する。

## A4-1: 出力レート音声のアセットキャッシュ

### 作業

- decode と output rate への変換を `AudioService` の background task にまとめる。
- cache 内の `DecodedAudio` は常に engine output rate とし、trim と gain curve を
  output-rate 基準で構築する。
- engine の `SetTrack` から sample rate と SRC worker を除き、完成済み track の
  差し替えだけを prep thread へ送る。
- sinc 設定を編集用途に十分な品質と bounded なコストへ調整し、既存の filter delay
  除去、tail flush、出力長の保証を維持する。

### 完了条件

- 44.1 kHz asset も engine には output rate の track として一度だけ届く。
- 同じ asset の複数レイヤーと同一レイヤーの再配置が SRC を再実行しない。
- 配置変更は次の mixer block で新しい `start_frame` を反映する。
- resampler の channel 分離、出力長、先頭遅延、末尾回収テストが通る。

## A4-2: Composition 終端の Pause 公開

### 作業

- `Transport::tick_with` が frame 変化だけでなく Playing → Paused の状態変化も
  `TransportUpdate` として返す。
- wall clock と audio clock の両方を同じ状態遷移規約にする。
- 終端 update を playback loop が公開し、既存の transport forwarding から
  `AudioCommand::Pause` を送る。

### 完了条件

- 最終 frame が既に公開済みでも、その次の終端 tick が `playing: false` を返す。
- wall/audio clock の両方で回帰テストが通る。
- 終端の位置は最終 frame に保持し、次の Play の既存 rewind 規約を変えない。

## A4-3: 音声準備状態と失敗の可視化

### 作業

- `AudioService` が準備中の layer / asset を問い合わせ可能にし、状態変化で notify する。
- Timeline の音声 layer bar と MediaBin の該当 asset row にローカライズした
  「準備中」を表示する。進捗率や modal UI は追加しない。
- decode 上限、offline、decode / SRC error を一回限りの service event として
  workspace へ送り、非自動消去の warning notification を表示する。

### 完了条件

- decode / SRC 中だけ Timeline と MediaBin に準備中表示が出て、完了または失敗で消える。
- 上限超過を含む準備失敗が asset id と原因を含む通知になる。
- render 中に状態変更、command dispatch、focus 変更を追加しない。

## A4-4: 文書と完了ゲート

### 作業

- 対象 issue を解消済みにし、ロードマップ、バックログ、計画 index を同期する。
- この計画を `done/` へ移し、標準検証と `ravel-review` を通す。

### 完了条件

- `cargo test -p ravel-audio`
- `cargo test -p ravel-app`
- `mise run check`
- PR 前の `ravel-review`
