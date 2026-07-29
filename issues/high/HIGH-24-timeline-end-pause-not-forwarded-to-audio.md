# [HIGH-24] Composition 終端の自動 Pause が音声エンジンへ転送されず、音が鳴り続ける

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-app / 再生トランスポート |
| 該当 | `crates/ravel-app/src/playback.rs:220-236`, `crates/ravel-app/src/playback.rs:239-259`, `crates/ravel-app/src/playback.rs:453-462` |

## 現状

`Transport::frame_from` は再生位置が Composition の長さを超えたとき
`PlaybackClock` を最終フレームで `pause()` し、**最終フレーム番号を返す**
（`playback.rs:246-253`。壁時計側の `PlaybackClock::current_frame`、
`ravel-core/src/runtime/playback.rs:88-95` も同じ動作）。

一方 `tick_with` は先頭で

```rust
let frame = self.frame_from(clock);
if frame == self.last_frame {
    return None;
}
```

としている（`playback.rs:224-227`）。終端に達した tick が返すフレームは
直前の tick で既に公開済みの `duration - 1` と**同値**なので、`None` が返る。
その結果 `spawn_tick_loop` の

```rust
if !update.playing {
    crate::audio::forward_transport(false, None, cx);   // playback.rs:460-461
}
```

に到達せず、エンジンへ `AudioCommand::Pause` が届かない。`SyncClock.playing`
は `true` のまま、prep スレッドはチャンクを作り続け、ミキサーはトラック終端まで
音を出し続ける。

トランスポート（UI 上の再生状態・プレイヘッド）は正しく停止するため、
**画は止まったのに音だけ鳴り続ける**という形で表面化する。

## 影響

Composition の長さより長い音声を置いた状態で最後まで再生するたびに発生する。
再生を押して放置するだけで踏む通常操作。停止するには手動で Pause / Stop が必要。

## 修正方針

`tick_with` を「フレームが動いていなくても、再生状態が Playing → Paused に
変化したら `TransportUpdate` を返す」形にする。`frame_from` が内部で
`pause()` する設計自体は残してよく、必要なのは状態変化の取りこぼしを塞ぐこと。
`ClockSource::Wall` 側にも同じ穴があるので両方が同じ経路で直る。

エンジン側へは `Pause` のみ送る（位置は最終フレームで凍結。次の Play は
`PlaybackClock::play` が先頭へ巻き戻すので追加のシークは不要）。

## 検証

- headless 回帰テスト: `Transport` + `SyncClock`（`ClockSource::Audio`）で
  最終フレーム公開後にサンプル位置を duration 超へ進め、
  `tick_with` が `playing: false` の update を返すことを確認
- `ClockSource::Wall` で同じ確認（既存 `tick_reports_auto_pause_at_the_end` は
  フレームが動くケースしか通っていない）
- 実機: Composition より長い音声で終端まで再生し、音が止まることを確認

## 関連

- [HIGH-12](HIGH-12-pause-does-not-stop-queued-audio.md) — Pause が届いた後の
  キュー済みチャンクの扱い（epoch 化で対応済み）。本件はその Pause 自体が
  送られない、より上流の穴
- [HIGH-23](HIGH-23-resampled-audio-not-cached.md) — 同じ実機セッションで発覚
