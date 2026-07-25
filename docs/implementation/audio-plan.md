# オーディオ実装計画（REQ-MEDIA-002 / REQ-MEDIA-003）

> **Status**: Draft — 2026-07-26 設計確定、未着手。
> `docs/implementation/media-import-plan.md` の単位 1（アセットモデル）完了後に着手する。

## 問題

`crates/ravel-audio` は CPAL 出力 / ミキサ / リサンプラ / エフェクト（gain・fade）
/ 同期クロック / 波形生成まで実装されているのに、**`ravel-app` から 1 行も
参照されていない**（`Cargo.toml` にも依存が無い）。音は一切鳴らない。

- **クロックが二重**。`ravel-audio` の doc コメントは
  「`SyncClock` が再生位置の単一の真実、映像レンダラがこれを読む」と宣言して
  いるが、実際の再生は `crates/ravel-app/src/playback.rs` の `Transport` が
  `Instant` から frame を算出して `PlaybackPosition` Global に publish している。
- **`Mixer::Track` に開始位置が無い**。`mix(frame_offset, frame_count)` は
  全トラックに同じオフセットを適用するので、全トラックが 0 秒開始の前提。
  効果音を任意の位置に置けない。
- **`Track.gain` は静的 `f32`**。`fade_in_frames` / `fade_out_frames` はあるが
  音量オートメーションは無い。
- **音声ノードが無い**。ただし `AudioBuffer` は既に `NodeData` 実装済みで
  `DataTypeId::AUDIO_BUFFER` を返すので、DAG に音声型自体は存在する。
- 音声ファイルを扱う UI が無い（メディアインポート計画側で MediaBin に載る）。

要件は REQ-MEDIA-002（CPAL + DSP、Must。受入条件のうち「同期再生」
「マルチトラックミキシング」「サンプルレート変換」「途切れない再生」は
ライブラリ単体では ✅ だが、アプリとして未達）、REQ-MEDIA-003
（オーディオリアクティブ = FFT + ビート検出 + BPM 同期、Should）。

## 決定事項（2026-07-26 設計セッション）

| # | 決定 | 根拠 |
|---|---|---|
| 1 | 音声は**レイヤーの殻**が持つ（`Layer.audio: Option<AudioSource>`）。ネットワークの `net.out` に audio ポートは足さない | 時間配置・in/out・solo/mute・親子・undo・選択・コピペ・ドラッグが既存レイヤー機構でそのまま効く。`net.out` 経由にすると `compile_composition` の殻チェーン拡張とブロック軸評価が必要になる |
| 2 | **AudioStore はバンクのみ**（時間配置を持たない）。配置は必ず音声レイヤー | トリガー列を持たせると選択モデル・undo 粒度・コピペ・Timeline 描画をもう一系統作ることになる。SFX の「大量」への対処は Timeline のグループ折りたたみ（UI のみ）で行う |
| 3 | バンクの UI は **MediaBin に統合**（種別フィルタ + タグ + 試聴）。専用パネルは作らない | `PanelKind` / キーバインド / ロケール / ワークスペースプリセットを 1 系統増やさない。分離は実際に数百入れてから判断する |
| 4 | **音声トラックがあり出力デバイスが開けたときは `SyncClock` を正**にし、`Transport` は従属側にする。音声なし / デバイスなしは現行の `Instant` にフォールバック | 「音が途切れなく再生される」（REQ-MEDIA-002 受入条件）を満たす側にクロックを合わせる。長時間再生でデバイスクロックと `Instant` はドリフトする |
| 5 | 解析ノードは**音声レイヤーを参照**（`layer.ref` / REQ-LAYER-005 の前例に合わせる） | レイヤーの `start_frame` / `in` / `out` を考慮して「今鳴っている位置」を解析できる。タイムライン上でズラしても反応がずれない。循環・欠落検出は既存の `validate::layer_ref_*` に乗る |
| 6 | 音量は **`AnimationChannel` でオートメーション**する | フェードだけでは足りない。`Mixer` にブロック単位の gain 評価を足す |
| 7 | DAG での音声**加工**（EQ / リバーブ / サイドチェイン）は非対象。v1 のノードは**解析専用** | `EvalContext` は `Copy` のフレーム軸専用構造体で、ブロック軸（start_sample / sample_count / sample_rate）を足すと ctx を触る全ノード・全テストに波及し、さらにリングバッファとプリロールの設計が必要 |
| 8 | 素材は**全長デコードしてメモリ常駐**（v1）。ストリーミングは非対象 | `Mixer::Track` が `Arc<[f32]>` 前提。5 分の 48kHz ステレオ f32 で約 115MB、効果音は無視できる。1 時間素材は非現実的なので上限超過時は警告する |

## 目標アーキテクチャ

### データモデル（ravel-core）

```rust
/// 殻が持つ音声ソース。映像を持たない音声レイヤーと、
/// 音声つき動画レイヤーの両方がこれを持つ。
pub struct AudioSource {
    /// media_assets のキー。映像と同じアセットテーブルを使う。
    pub asset_id: String,
    /// コンテナ内の音声ストリーム番号（動画の音声トラック選択）。
    pub stream_index: usize,
    /// 音量（0.0–）。キーフレーム可能。
    pub gain: AnimationChannel,
    pub fade_in_frames: u64,
    pub fade_out_frames: u64,
    /// 音声だけを消す（映像は残す）。レイヤーの mute とは独立。
    pub audio_muted: bool,
}

pub struct Layer {
    // 既存の殻フィールド …
    pub audio: Option<AudioSource>,
}
```

- 時間配置は**レイヤーの `start_frame` / `in_frame` / `out_frame` をそのまま使う**
  （音声専用のフィールドを作らない）。チャネルはレイヤーローカルフレームで
  評価する（REQ-LAYER-006）。
- 音声レイヤーは `has_frame_output() == false`（ネットワークは In/Out だけの
  空ネットワーク）。合成チェーンには参加せず、殻の Transform ノードだけが
  親子付けのために残る既存の null レイヤー経路に乗る。
- 動画レイヤーの音声は**明示**。インポート時の「レイヤーとして追加」が
  ネットワークの `media` ノードと殻の `AudioSource` に同じ `asset_id` を設定する
  （ネットワークを走査して「音声つきメディアノードを探す」暗黙解決はしない —
  ノードが複数ある場合に曖昧になる）。
- バンクのメタデータ（タグ / カテゴリ / お気に入り）は
  `MediaAssetEntry.metadata` の拡張として持つ。**時間情報は持たない**。

### ミックスダウン経路

```text
Document（audio を持つレイヤー群）
        │  ProjectState の document observer
        ▼
AudioMixdown::build(comp, frame_rate)      ravel-app/src/audio/mixdown.rs
        │  レイヤー → Track { samples, start_frame, gain 曲線, fades, mute/solo }
        │  デコードは background executor、結果は asset_id + stream で Arc キャッシュ
        ▼
AudioEngine::SetTrack …                    ravel-audio
        │  prep スレッドでブロック mix（gain はブロックごとにチャネル評価）
        ▼
CPAL callback → SyncClock::advance()
```

- レイヤーの `muted` / `solo` は**映像と音声で同じ意味**にする
  （mute したレイヤーは音も消える。音だけ消したいときは `audio_muted`）。
  ソロは既存の `active_layers` と同じ規則（どれかが solo なら solo のみ）。
- 親子付けは音声に効かない（親の変換は音に意味を持たない）。
- レイヤーの追加/削除/時間移動は Document の変更なので、observer から
  差分を見て `SetTrack` / `RemoveTrack` を送る。**再生中の編集で音が途切れない**
  ことを確認する（`SetTrack` は prep スレッド側で次ブロック境界に適用）。

### クロック

```text
音声トラックあり + デバイス開ける:
  CPAL callback → SyncClock.advance(samples)
                       │  frame = floor(samples / rate × fps)
  timer(frame_interval) → Transport::tick(ClockSource::Audio(&SyncClock))
                       → PlaybackPosition → 評価要求

音声なし / デバイスなし（CI・ヘッドレステスト含む）:
  timer(frame_interval) → Transport::tick(ClockSource::Wall(Instant))
```

`Transport` に `ClockSource` を導入し、`tick(now)` の代わりに
`tick(&ClockSource)` で現在フレームを取る。既存の `Instant` 経路は
`ClockSource::Wall` として保持する（テストは全部こちらを使う）。
seek / play / pause は `Transport` から `AudioEngine` へ送り、
`SyncClock` を seek 位置へ合わせる。

### 解析ノード（REQ-MEDIA-003 の入口）

```text
audio.analysis(layer: LayerId, mode: Rms|Peak|Band, band: Low|Mid|High) → Scalar
```

- 参照するレイヤーの `AudioSource` を Document から解決し、そのレイヤーの
  ローカル時間で窓を切って解析する。`VideoProcessor` と同じく
  プロセッサ内部で `Mutex` つきキャッシュにデコード済みバッファを持つ
  （再生用バッファとメモリが 2 重になるのは v1 の許容コスト。共有キャッシュは v2）。
- 出力は `Scalar` なので、既存の**パラメータ入力ポート**（param-input-ports）
  経由で任意のパラメータを駆動できる。REQ-MEDIA-003 の「解析結果が統一
  アニメーションチャネルに接続できる」はこの経路で満たす。
- BPM 検出・ビートマーカー・キーフレームの BPM スナップは v1 非対象
  （解析の土台ができてから別計画）。
- FFT は MIT ライセンスのクレート（`rustfft` 等）を使う。GPL の `aubio` は
  REQ-MEDIA-003 の明示的な禁止事項。**依存追加はユーザー承認が必要**
  （`.agents/rules/rust.md`）。

### 波形

- `WaveformData::generate` は既にあるので、生成は `Waveform` キャッシュ
  （メディアインポート計画のサムネイルと同じ外部グローバルキャッシュ、
  キー = 絶対パス + mtime + size + セグメント長）に置く。
- 表示先は 2 つ: MediaBin の音声行のインライン波形と、Timeline の
  音声レイヤーバー内の波形。どちらも読み取り専用の描画。
- `PanelKind::Waveform`（波形モニタスコープ）は**別物**なので触らない。

## 実装単位

各単位が 1 PR。単位 1 → 2 → 3 は順序依存、4 以降は並行可能。

1. **音声レイヤーのデータモデル**（ravel-core / ravel-ui / assets）
   `AudioSource` + `Layer.audio`、永続化（format v4 のフィールド追加。
   メディアインポート計画の v4 と同一バージョンに載せるか、独立して v5 にするかは
   着手時点の実装状況で決める）、`audio` レイヤーテンプレート、
   Properties の Audio セクション（gain のキーフレーム、fade、audio_muted、
   ストリーム選択）、`CommandId::LayerAddAudio`。
   REQ-LAYER-001 の殻プロパティ列挙に `audio` を追記する。
2. **ravel-audio の拡張**（ravel-audio）
   `Track.start_frame`、`mix` の per-track オフセット、
   ブロック単位 gain（`Vec<f32>` の gain 曲線か評価コールバック）、
   トラック差分適用 API（`SetTrack` / `RemoveTrack` の冪等化）。
   単体テストで「オフセットつきミックス」「gain 曲線の適用」「差分適用で
   出力が途切れない」を固定する。
3. **再生配線**（ravel-app）
   `ravel-audio` 依存の追加、`AudioEngine` の起動と寿命管理、
   `AudioMixdown` の構築と document observer からの差分送出、
   `Transport` の `ClockSource` 化と `SyncClock` 従属、
   デバイスが無い環境でのフォールバック。
4. **動画レイヤーの音声**（ravel-app / ravel-ui）
   インポート時に `AudioSource` を設定、ストリーム選択 UI、
   音声つき動画をタイムラインに置いて音が出ること。
5. **波形表示**（ravel-app / ravel-ui）
   波形キャッシュ、MediaBin のインライン波形、Timeline の音声レイヤーバー。
6. **解析ノード**（ravel-nodes / ravel-core）
   `audio.analysis`（RMS / ピーク / 帯域エネルギー）、レイヤー参照、
   `validate` の循環・欠落チェックへの追加、FFT クレートの依存追加（要承認）。
7. **バンクとしてのタグ・試聴**（ravel-ui / ravel-app）
   MediaBin の音声フィルタ、タグ付け、行の試聴再生（`AudioEngine` の
   一時トラック）、`docs/ui-impl-status.md` / REQ-MEDIA-002 / REQ-MEDIA-003 の更新。

## 完了条件（単位別）

- **単位 1**: 音声レイヤーを作って保存 → ロードで `AudioSource` が保持される。
  gain のキーフレームが Properties から打てて undo が 1 段。
  音声レイヤーが合成チェーンに入らない（`compile_composition` の結果に
  Merge が増えない）。
- **単位 2**: 開始位置つきの 2 トラックが正しい位置でミックスされる。
  gain 曲線が適用される。トラック差分適用の前後でブロック境界に不連続が出ない。
- **単位 3**: 音声レイヤーを置いて再生すると音が出る。playhead が
  `SyncClock` に追従する（デバイスあり）。デバイスが無い環境で
  再生が現行どおり動く（CI で回るテストはすべてこちら）。再生中に
  レイヤーを動かしても音が途切れない。
- **単位 4**: 音声つき動画を D&D → レイヤー化 → 映像と音がフレーム同期で出る。
  ストリームを切り替えると鳴る音が変わる。
- **単位 5**: MediaBin と Timeline の両方に波形が出る。2 回目の表示で
  デコードが走らない。
- **単位 6**: `audio.analysis` の出力をパラメータ入力ポートに繋ぐと、
  再生位置に応じて値が動く。参照レイヤーを削除しても評価が失敗しない。
  レイヤーを時間移動すると解析窓も移動する。
- **単位 7**: 効果音を 100 件インポートしてフィルタと試聴が実用的に動く。
  Timeline の音声レイヤー群が折りたためる。

## 検証

- `mise run check`。
- 音声デバイスに依存するテストは CI で回せないので、`ClockSource::Wall` と
  スタブしたエンジン（`AudioCommand` の送出内容を検証する）でカバーする。
  実デバイス経路は手動確認手順を PR に明記する。
- 実機: 音声レイヤーの再生 / 動画の音 / 再生中の編集 / 100 件の効果音の
  試聴とフィルタ。

## 非対象

- DAG での音声処理（EQ / リバーブ / サイドチェイン / 音声エフェクトノード）。
  `EvalContext` へのブロック軸追加が前提になるので別計画。
- AudioStore のトリガー列（時間を持つバンク）。SFX をレイヤーで並べた
  使い心地を見てから判断する。
- ストリーミング再生（全長メモリ常駐で始める）。
- BPM 検出 / ビートマーカー / キーフレームの BPM スナップ（REQ-MEDIA-003 の後半）。
- 音声の書き出し（エクスポート機能自体が未実装。`ravel-media` の `Encoder` は
  音声 mux に対応済みなので、エクスポート計画側で拾う）。
- VST3 / CLAP ホスティング、EQ（REQ-MEDIA-002 の将来項目）。
- マルチチャンネル（5.1 等）とパンニング。v1 はステレオ出力のみ。
