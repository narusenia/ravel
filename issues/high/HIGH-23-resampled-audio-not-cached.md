# [HIGH-23] リサンプル結果が未キャッシュで、レイヤー編集ごとに全長リサンプルをやり直す（配置・トリムが効かない / 初回再生が数分無音）

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-audio / エンジン + ravel-app / AudioService |
| 該当 | `crates/ravel-audio/src/engine.rs:476-497`, `crates/ravel-audio/src/engine.rs:558-622`, `crates/ravel-app/src/audio/mod.rs:222-303` |

## 現状

`handle_command(SetTrack)` は `sample_rate != output_rate` なら**必ず**
`ResampleJob` を投入する。リサンプル結果はどこにもキャッシュされないため、
同じアセットに対する 2 回目以降の `SetTrack` もトラック全長を最初から
変換し直す。

`AudioService::sync` は配置だけの変更（タイムラインのバー移動、トリム、
mute / solo / フェード）でも `built` トラックの `start_frame` 等を差し替えて
`SetTrack` を再送するため（`audio/mod.rs:229-253`）、**レイヤーを 1 フレーム
動かすだけで全長リサンプルが走る**。しかも `ResampleQueue` は同一トラックの
先行ジョブを supersede して捨てるので、完了間際の変換結果も破棄される。

リサンプルが完了するまでの間ミキサーの状態は次のどちらかになる。

- **初回**（まだトラックが無い）: ミキサーにトラックが存在せず**完全に無音**。
- **再送**（既にトラックがある）: **古い配置のトラックがそのまま鳴り続ける**。
  ユーザーには `start_frame` / `in_frame` / `out_frame` の編集が
  「無視されている」ように見える。

44.1kHz → 48kHz は音楽ファイルのほぼ全件が該当するので、常時この経路に入る。

## 実測（このリポジトリ、`resample_buffer` 44.1kHz→48kHz stereo）

| ビルド | 音声 1 秒あたり | 60 秒の音声 | 4 分の曲 |
| --- | --- | --- | --- |
| debug | 0.307 s | 18.4 s | 約 74 s |
| release | 0.0033 s | 0.20 s | 約 0.8 s |

debug は release の約 92 倍遅い。`sinc_len: 256` / `oversampling_factor: 256`
（`resampler.rs:55-61`）が過剰品質で、この係数の主因。開発中は debug バイナリを
起動するため、「再生を押しても数十秒〜数分無音、しばらくすると鳴り出す」
「レイヤーを動かしても効かない」がそのまま日常の症状になる。

## 影響

- 44.1kHz 素材の初回再生が debug で数十秒〜数分無音。
- 音声レイヤーの配置・トリム編集が反映されない（見かけ上のバグに化ける）。
- 編集操作ごとに CPU コア 1 本を数十秒占有する。

なお `SetTrack` ごとの `invalidate_prepared_audio`（`engine.rs:553-556`）が
epoch を進めてキュー済みチャンクを捨てるため、ドラッグ中は本質的に音が途切れる。
こちらは HIGH-23 を直しても残る（差分送信の粒度の問題）。

## 修正方針

1. **アセット単位に出力レートへ一度だけ変換してキャッシュする。** デコード完了時に
   ravel-app のバックグラウンドエグゼキュータでリサンプルし、
   `DecodedAudio` を出力レートで保持する。`AudioMixdown::build_track` のトリムは
   出力レート基準に変わり、`SetTrack` は常に `sample_rate == output_rate` で
   送られる。エンジンからリサンプル経路を外す（HIGH-15 の修正方針が本来
   意図した終着点）。これで配置・トリム編集はリサンプル無しで即時反映になる。
2. **リサンプラ自体のコストを下げる。** `rubato::FftFixedIn` に替えるか、
   `sinc_len` / `oversampling_factor` を 64 / 32 程度へ落とす。
   `MED-MED-03`（フィルタ遅延・テール欠落）と同じ関数なので同時に手を入れる。
3. 出力レート変換の完了までは対象レイヤーを「準備中」として UI に出す
   （`MED-AUD-03`）。

なお `prep_thread_main` は prepared 結果をコマンドより**先に**drain する
（`engine.rs:384-402`）。新しい `SetTrack` が既にコマンドキューに載っている状態で
古い世代の変換結果が届くと、世代が上がる前に古い配置のトラックがミキサーへ入る。
次の変換が終われば正しい状態に収束するので恒久的な誤りではないが、
「編集が効かない」体感を 1 世代ぶん長くする。方針 1 でエンジンから
リサンプル経路が消えればこの順序問題も消える。残す場合はコマンドを先に
処理する（または投入時点で世代を確定させる）こと。

## 検証

- 同一アセットへの 2 回目以降の `SetTrack` でリサンプラが呼ばれないテスト
- 44.1kHz レイヤーの `start_frame` 変更が 1 ミックスブロック内で反映されるテスト
- 実機: debug ビルドで 44.1kHz ファイルを D&D → 再生開始までの時間を測る

## 関連

- [HIGH-15](HIGH-15-settrack-resamples-on-prep-thread.md) — 同じ `SetTrack` 経路。
  リサンプルを prep スレッドから外す修正は入ったが、**やり直しの回数**は
  手つかずだった
- [medium/media-audio.md](../medium/media-audio.md) `MED-MED-03` — 同じ
  `resample_buffer` のフィルタ遅延・テール欠落
- [medium/media-audio.md](../medium/media-audio.md) `MED-AUD-02` / `MED-AUD-03` —
  デコード上限超過の無言の無音、準備中フィードバックの不在
