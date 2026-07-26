# ravel-audio 拡張（audio-plan 単位 2）HANDOFF

## 変更概要（ファイル単位）

- `crates/ravel-audio/src/mixer.rs`
  - `Track.start_frame` を追加し、出力窓とトラック範囲の交差部分だけをミックスするよう変更。
  - fade の評価位置を出力タイムラインではなくトラックローカルフレームへ修正。
  - `TrackGain::{Constant, Curve}` とフレーム単位の gain 評価を追加。
  - 同一 ID を原位置で置換する `Mixer::set_track` を追加し、`add_track` も ID 一意性を保つよう変更。
  - オフセット、gain 曲線、境界更新、空入力などの単体テストを追加。
- `crates/ravel-audio/src/engine.rs`
  - `AudioCommand::SetTrack` が完全な `Track` とそのサンプルレートを受け取るよう変更。
  - `SetTrackGain` が `TrackGain` を受け取るよう変更。
  - prep スレッドの mix 前コマンド排出点を、再生中のトラック更新が可視化される準備済みブロック境界として明文化。
  - `SetTrack` / `RemoveTrack` の冪等性テストを追加。
- `crates/ravel-audio/src/lib.rs`
  - `TrackGain` を crate root から re-export。

コミット:

- `b23cf12 feat: give mixer tracks a start frame on the output timeline`
- `fdad98c feat: evaluate per-block gain curves in the mixer`
- `e2cb4cd feat: apply track updates idempotently at block boundaries`

## 選んだ設計とその理由

### gain 曲線

`TrackGain::Curve(Arc<[f32]>)` の事前サンプリング済み曲線を採用した。`ravel-audio` が `AnimationChannel` や `ravel-core` の評価方法を知る必要がなく、任意クロージャも保持・実行しない。mix は現行どおり prep スレッドで行い、CPAL コールバックへ評価処理を持ち込まない。

曲線はトラックローカルのサンプルフレームで添字付けする。曲線末尾を超えた場合は最後の値を保持し、空曲線は安全な既定として unity (`1.0`) とした。処理順は「トラック gain → トラック fade → 各トラックの加算 → master gain」と doc comment で固定した。

### 差分適用の境界

prep スレッドは pending command を各 `Mixer::mix` の直前にだけ排出するため、`SetTrack` / `RemoveTrack` は次に準備する完全なブロックから可視化される。`Mixer::set_track` は同一 ID の要素を一回の代入で置換し、remove/add の中間状態を作らない。`RemoveTrack` は対象が無くても成功扱いの no-op である。

これは「次の準備済みブロック境界」であり、既に CPAL 向けキューに積まれた旧ブロックは置換しない。したがって実際に聞こえる反映時刻には `queue_depth` 分までの既存レイテンシがあるが、ブロック途中の差し替えや無音ブロックの挿入は起こらない。

### fade のローカル化

以前は `Mixer::mix` の出力 `frame_offset` を fade に渡していたため、開始位置を追加すると timeline 0 基準で fade が進んでしまう。交差開始から `track.start_frame` を引いた `track_frame_offset` を fade-in / fade-out の双方へ渡し、トラックの実サンプル先頭・末尾を基準にした。

## 実行した検証と結果

### `cargo test -p ravel-audio`

```text
running 59 tests
test result: ok. 59 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s

Doc-tests ravel_audio
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### `mise run check`

初回の全出力確認後、キャッシュ済み状態で成功結果を抽出した再実行の出力:

```text
[lint:patterns] lint-patterns: clean
[lint:patterns] Finished in 296.1ms
[fmt] Finished in 898.4ms
[clippy]     Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.28s
[clippy] Finished in 2.58s
[test]     Finished `test` profile [unoptimized + debuginfo] target(s) in 2.73s
[test] test result: ok. 317 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.72s
[test] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s
[test] test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
[test] test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.67s
[test] test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.13s
[test] test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s
[test] test result: ok. 59 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
[test] test result: ok. 316 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s
[test] test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
[test] test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
[test] test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 140 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
[test] test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
[test] test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
[test] test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
[test] test result: ok. 190 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s
[test] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
[test] Finished in 7.64s
Finished in 7.65s
```

## 判断したこと一覧

- `Track.start_frame` は既存の `Mixer::mix` / `Track::frame_count` と自然に演算できる非負の `usize`（サンプルフレーム）とした。
- gain は `Arc<[f32]>` の事前サンプリング方式にし、評価コールバック方式は採用しなかった。
- 空の gain 曲線は unity、短い曲線は末尾保持、長い曲線はトラック範囲外を評価しない。
- gain / fade / master の乗算順を track gain → fades → sum → master に固定した。
- `Mixer::add_track` でも ID を一意に保ち、同一 ID は置換するようにした。
- `AudioCommand::SetTrack` は個別フィールドの列挙ではなく完全な `Track` を受け取り、start/fade/gain/mute/solo を一括かつ原子的に差し替えられる形にした。
- 差分適用境界は新しいキューを増やさず、既存 prep ループの「コマンド排出後、mix 前」を使った。
- `RemoveTrack` の対象なしはエラーにせず、既存の bool 戻り値 `false` と command 側 no-op を維持した。新しい失敗条件が無いため `AudioError` は追加していない。
- 連続性は「更新処理が中間の無音ブロックやブロック内切替を作らない」こととして固定し、信号内容そのものを変形する自動クロスフェードは追加しなかった。

## 既知の制約 / 積み残し

- GUI / 実オーディオデバイス確認は指示どおり未実施。自動テストのみ。
- `start_frame` は非負。負の composition layer 開始位置は、単位 3 の `AudioMixdown` で先頭サンプルをクリップして output frame 0 へ配置する必要がある。
- gain 曲線は出力サンプルフレーム単位で事前生成する必要がある。`AnimationChannel` からのサンプリングは単位 3 側の責務。
- 任意に波形が異なるトラックへの置換や非ゼロ点での削除には、信号そのものに由来する段差があり得る。今回の API は更新処理由来の空白を防ぐが、自動クロスフェードは計画に無いため行わない。
- 既に CPAL キューへ送ったブロックは差し替えないため、更新反映には既存の queue depth 相当のレイテンシがある。

## 既存 API を破壊的に変えた箇所

- `Track.gain: f32` → `Track.gain: TrackGain`。
- `AudioCommand::SetTrack { id, samples, sample_rate, channels }` → `AudioCommand::SetTrack { track, sample_rate }`。
- `AudioCommand::SetTrackGain { gain: f32 }` → `AudioCommand::SetTrackGain { gain: TrackGain }`。
- `Track` の public field に `start_frame` を追加したため、`Track::new` ではなく struct literal を使う外部コードは追従が必要。
- `Mixer::add_track` は同一 ID の重複追加ではなく置換するようになった（シグネチャ変更なしの挙動変更）。

`Mixer::mix(&self, usize, usize)` のシグネチャは変更していない。
