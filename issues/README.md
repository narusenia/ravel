# Ravel — 課題インデックス

コードベース全体（8クレート、約9.1万行）を対象に、技術的負債・パフォーマンス問題・
バグを網羅調査した結果。**全項目はソース上で実物を確認済み**（仮説・スタイル指摘は除外）。

調査範囲: `ravel-core` / `ravel-nodes` / `ravel-gpu` / `ravel-media` / `ravel-audio` /
`ravel-i18n` / `ravel-ui` / `ravel-app`、および `assets` のロケールデータ。

| 深刻度 | 未解決 | 解決済み | 未解決分の場所 |
| --- | --- | --- | --- |
| critical | 0 | 4 | — （全件解決） |
| high | 2 | 33 | [high/](high/) — 1件1ファイル |
| medium | 29 | 45 | [medium/](medium/) — 領域別5ファイル |
| low | 33 | 9 | [low/backlog.md](low/backlog.md) — 1ファイル |

解決済みの項目は個票を **[closed/](closed/)** へ移す。個票の中身は起票時のまま
残し、各項目の `**解決済み**` 行が結果と PR 番号を記録する。critical / high は
1件1ファイルなのでファイルごと移し、medium / low は該当項目だけを
`closed/medium-*.md` / `closed/low.md` へ切り出す。

**この索引は未解決分だけを扱う。** 解決済み項目の一覧は
[closed/README.md](closed/README.md)。

> **着手順の正は `docs/implementation/roadmap.md`。** この文書は「何が壊れて
> いるか」と「どの項目が互いに絡んでいるか」の台帳で、**順序を決めない**。
> ロードマップは issue をクラスタ単位でフェーズに割り当てており、下記の
> クラスタ名がそのまま対応する。個票（原因・該当行・修正方針）はここにある。

| クラスタ | ロードマップ上の位置 |
|---|---|
| データ保全 + 無言の失敗 + latent クラッシュ | 完了（フェーズ A2「失われないこと」） |
| 音声・A/V 同期 | 完了（フェーズ A3「音声の正しさ」） |
| 音声の準備と停止（実機で発覚） | 完了（フェーズ A4「音声が編集に追いつくこと」） |
| NodeEditor 固有の再描画 + 操作の即効修正 | 完了（フェーズ A） |
| もっさり 第1段（評価・レンダー回数） | 完了（`done/ui-responsiveness-plan.md`） |
| もっさり 第2段（描画1回あたり） | フェーズ H（`gpu-compositing-plan.md`。`HIGH-05` は済） |
| もっさり 第3段（評価器のアルゴリズム）+ パネル1回あたり | 完了（フェーズ C3「応答性の残り」） |
| もっさり 第4段（メディア・スクラブ） | フェーズ C2（`cache-plan.md`）と C3 |
| 設定が効かない / キーバインド上書き | フェーズ C（`settings-screen-plan.md`） |
| 操作の正しさ（Timeline / NodeEditor / 選択） | フェーズ E |
| 色管理（合成が非線形空間） | フェーズ CM（`color-management-plan.md`） |
| 構造的負債（未使用サブシステムの判断） | フェーズ H |
| 軽微（low） | 随時 |

---

## UI / 描画のもっさり — 原因の分解

体感の遅さは単一原因ではなく、**評価回数の爆発 × 1回あたりのコスト**の積。
以下の「段」は**原因の分解であって着手順ではない**（順序は
`docs/implementation/roadmap.md`）。段の名前は他文書からの参照があるので
維持している。

### 第1段: 評価・レンダー回数を減らす（変更は小さく効果は最大）

**完了**（PR #191 / #192 / #193）。設計と実装単位は
`docs/implementation/done/ui-responsiveness-plan.md`（RESP-1〜3）。
ただし実測の結論は「体感の主因は第1段ではなく第2段」だった。

1. **[CRIT-01](closed/CRIT-01-eval-update-notifies-whole-workspace.md)** — 解決済み
   評価結果ごとに全5パネルがモデル再構築 + 再レンダー。再生中は毎フレーム。
   これが他のすべてのコストに掛かる倍率になっている。実質1箇所の修正。
2. **[HIGH-07](closed/HIGH-07-document-changed-cascade-per-mouse-move.md)** — 解決済み
   マウス移動ごとに `document_changed` の全カスケード（選択プルーン、音声同期、
   コンパイル破棄、5パネル notify、選択グローバル再 publish の第2波）。
3. **[HIGH-06](closed/HIGH-06-pipeline-recompiled-per-param-edit.md)** — 解決済み
   スライダードラッグ中に GPU コンピュートパイプラインを毎回再コンパイル（naga 再検証込み）。
   ただし実測では tick の約23%で、「編集中の重さ」の主因ではない（第2段が主因）。
   詳細は `docs/implementation/perf-baseline.md`「RESP-3 完了時」。

### 第2段: 描画1回あたりのコストを削る

**実測上の主因はここ**。設計と実装単位は
`docs/implementation/gpu-compositing-plan.md`（GPUCOMP-1〜11）。

4. **[HIGH-05](closed/HIGH-05-shell-chain-cpu-per-pixel.md)** — 解決済み（2026-07-29）
   シェル合成チェーン（transform / opacity / merge）が CPU per-pixel のため、
   レイヤーごとにブロッキング GPU リードバックを強制していた。GPUCOMP-2/3/5/6 で
   3プロセッサすべてに GPU 版が入り、シェルチェーン由来のリードバックは 0 になった。
   チェーン全体の回帰 pin は GPUCOMP-7。
5. **[HIGH-04](closed/HIGH-04-per-frame-blocking-readback.md)** — 解決済み（2026-08-05）
   リードバックそのものが最悪実装（毎回ステージング確保 + デバイス全体待ち + 二重コピー）。
   `GPUBK-6` がステージングをサイズ別プールに載せ、待ちを対象 submission に絞り、
   二重コピーを消した。1080p 6.13 → 2.4 ms、4K 26.89 → 6.2–7.6 ms。
6. **[HIGH-08](closed/HIGH-08-ui-thread-f32-to-bgra-conversion.md)** — 解決済み（2026-08-05）
   UI スレッドでの全フレーム色変換。`GPUCOMP-9` が変換を評価ワーカーへ移し、
   `ViewerFrame` は BGRA の完成画像を運ぶ形になった。UI スレッド占有は
   1024×576 で 1.21 ms → 0、1080p で 4.33 ms → 0。
7. **[HIGH-09](closed/HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md)** — 解決済み（2026-08-12、#391）
   GPU→CPU→GPU 往復 + アトラス churn。色変換の側は `HIGH-08` が解き、往復は
   `zero-copy-viewer-plan.md` が macOS / Linux / Windows すべてで消した。

### 第3段: 評価器のアルゴリズム的コスト

**解決済み**（フェーズ C3、`RESP3-1`〜`RESP3-4`）。設計と実装単位は
`docs/implementation/responsiveness-stage3-plan.md`。

8. **[HIGH-01](closed/HIGH-01-evaluator-no-adjacency-index.md)** — 解決済み（2026-08-13、#395）
   隣接インデックスが無く、ノード訪問ごとに全エッジ走査（1回の pull が O(N·E)）。
   `Evaluator` が `Graph::ptr_eq` をキーにスコープ単位の隣接インデックスを持つ形にして、
   1,000 ノード / 1,497 エッジでコールドプル 18.7 → 0.8 ms、dirty 再プル 18.8 → 0.27 ms。
9. **[HIGH-02](closed/HIGH-02-graph-eq-no-ptr-eq-fastpath.md)** — 解決済み（2026-08-13、#395）
   編集ごとに全レイヤーネットワークを deep compare（`Arc::ptr_eq` の高速路が無い）。
   `changed_network_paths` が `Graph::ptr_eq` で短絡し、`Graph::eq` も 3 段の短絡を持ち、
   祖先チェーンは親索引経由の O(1) ルックアップになった。
10. **[HIGH-03](closed/HIGH-03-params-resolved-per-visit.md)** — 解決済み（2026-07-31）
   キャッシュヒット時でもパラメータ全再解決、`PathPoints` を毎フレーム clone。
   → `docs/implementation/cache-plan.md` の CACHE-2 が回収した。

### 第4段: メディア・スクラブ

11. **[HIGH-16](closed/HIGH-16-no-decoded-frame-cache.md)** — 解決済み（2026-08-10）
    デコード済みフレームキャッシュが無く、逆方向スクラブと再描画で GOP を丸ごと再デコード。
    → `docs/implementation/cache-plan.md` の CACHE-8 が回収した（`ravel-media` の
    アセット単位共有キャッシュ、予算は `CacheKind::MediaFrame`）。
12. **[HIGH-32](closed/HIGH-32-linear-ingest-powf-per-pixel.md)** — 解決済み（2026-08-11、#378）
    線形 ingest が画素ごとに f64 の transfer function を評価し、直列で舐めていた。
    厳密な LUT（u8 は 256 要素、u16 は 65,536 要素）と primaries 行列のキャッシュ、
    float 経路の rayon 行分割で **sRGB 7 倍 / PQ 13 倍**。**行列は票に無かった第二の
    真因**で、`invert` が画素ごとに `Vec` を 24 回確保していた（`cc7e194` が HDR を
    Rec.2020 に解決するまで恒等行列しか通らず露出していなかった）。
13. **[HIGH-17](closed/HIGH-17-sws-scaler-recreated-per-frame.md)** — 解決済み（2026-08-11、#380）
    sws スケーラをフレームごとに再生成。**票の「デコード経路の CPU 時間を支配する」は
    実測で成立しなかった** — 1080p の内訳は ingest 89% / scale 4% / デコード 6% で、
    このキャッシュの取り分は **-0.8%**。初回に出た -6.4% は計測順バイアスだった
    （順序を入れ替えると符号が反転する）。同時に、出力バッファのプールが
    `CacheBudget` の会計を壊していたので取り下げた。数字は小さいが、毎フレームの
    フィルタテーブル構築は消え、**再利用スケーラの画素同一性テスト**が入った。

### 独立: NodeEditor 固有の再描画（第1段の効果を打ち消していた）

**解決済み**（フェーズ A、`HIGH-21` / `HIGH-22`）。

14. **[HIGH-21](closed/HIGH-21-node-editor-repaints-every-playback-frame.md)**
    **解消（2026-08-02 再調査 → 2026-08-03 修正）。** 当初挙げた 3 原因のうち
    2 つ（`NodeEvalTimings` の無条件 notify、`add_node_menu_model` の毎 render
    再構築）は再調査の時点で既に直っており、3 つ目（`shape_line` がノード毎・
    ポート毎）は誤診だった — gpui の `layout_line` が 2 フレーム分キャッシュ
    するので、文字列が変わらないラベルとポート名は当たる。
    **実際の主因は再描画のゲートの粒度**で、表示は既に丸めてあるのに notify の
    判定がナノ秒精度の `Duration` を見ていた。表示テキストと色帯を組にした値で
    ゲートし、`categories` / `labels` をグラフ変更時だけ作り、
    `NodeEvalTimings` を構造変更ごとに刈るようにして解消した。
    残るのは修正方針 4（`Rc` 化）だけで、これは**実測してから判断する**もの。

### 補足

パネル側の1回あたりコストは
[medium/ui-rendering.md](medium/ui-rendering.md)（未解決分）と
[closed/medium-ui-rendering.md](closed/medium-ui-rendering.md)（解決済み）に分かれている。
フェーズ C3 で解決したのは `MED-UI-01`（毎編集の再コンパイル）、
`MED-UI-03`（Timeline の垂直カリング）、`MED-UI-04`（Timeline の revision ゲート）、
`MED-UI-06`（2 経路の重複 sync）。`MED-UI-05`（Outliner / MediaBin の全行再構築）は
Outliner 側が #397、MediaBin 側が #400 で解決した。`MED-UI-02` は二段で閉じた —
`RESP3-7` がプレイヘッドの空振りを、`VIS-2`〜`VIS-4` が「裏のタブでは
更新を遅らせ、表に戻ったときに追いつく」を入れた。**この節の未解決は
`MED-UI-07`（狭い Properties の Vector 行）だけ**である。
第1段を直すと呼ばれる回数は減るが、レイヤー数が増えるとこれらが再び効いてくる。

---

## データ保全（解決済み、フェーズ A2）

- **[CRIT-02](closed/CRIT-02-save-failure-invisible-and-swallows-quit.md)**
  保存失敗が完全に不可視。しかもガード付き保存の失敗で Quit / Close が無言で破棄される
- **[CRIT-03](closed/CRIT-03-project-write-not-atomic.md)**
  保存が truncate → write の非アトミック。クラッシュで `.ravprj` が破損し、
  `.bak` へのフォールバック経路も無い
- **[CRIT-04](closed/CRIT-04-uncommitted-gesture-baked-by-foreign-commit.md)**
  ペン / ドラッグの未コミット状態が他パネルのコミットで焼き込まれ、Esc が無効化される
- オートセーブとクラッシュ復旧ジャーナルはどちらも未配線
  → [medium/app-shell.md](medium/app-shell.md) MED-APP-10 / MED-APP-11、
  [medium/core-evaluator.md](medium/core-evaluator.md) MED-CORE-08

保存失敗が見えず・保存自体が非アトミック・オートセーブもジャーナルも無いという3点が
同時に成立していたので、この4件は独立した1エピックとして扱った。
**フェーズ A2「失われないこと」がこのエピック**で、無言の失敗（`HIGH-18` /
`HIGH-20` / `MED-APP-12`）と latent クラッシュ（`MED-GPU-03` / `LOW-GPU-01`）も
同じフェーズで解決した。**`MED-CORE-04` は一部のみ**で、評価の再帰には
上限が入ったがデシリアライズ経路は無防備なまま残っている（2026-08-03 再判定）。
**オートセーブとジャーナル
（`MED-APP-10` / `MED-APP-11` / `MED-CORE-08`）はこのフェーズの対象外で、
未解決のまま残っている。**

---

## 音声・A/V 同期（解決済み、フェーズ A3）

[HIGH-12](closed/HIGH-12-pause-does-not-stop-queued-audio.md)（Pause でキューが止まらない）、
[HIGH-13](closed/HIGH-13-seek-does-not-flush-audio-queue.md)（Seek で flush しない）、
[HIGH-14](closed/HIGH-14-clock-advances-over-underrun.md)（アンダーラン中もクロック進行）、
[HIGH-15](closed/HIGH-15-settrack-resamples-on-prep-thread.md)（SetTrack が prep スレッドをブロック）
は互いに増幅し合っていた。SetTrack のブロックがアンダーランを起こし、
アンダーランがクロックドリフトになり、Pause / Seek がそれぞれ固定オフセットを追加する。
個別に直すより、チャンクキューに epoch を導入する設計変更で4件同時に解けた。
**フェーズ A3「音声の正しさ」**がこの塊で、`MED-MED-03/04/05` と `MED-AUD-01`
も同じフェーズで解決した。

音声デコーダ側の [HIGH-10](closed/HIGH-10-audio-chunk-seek-wrong-time-base.md) /
[HIGH-11](closed/HIGH-11-audio-chunk-no-trim.md) は、
ストリーミング音声再生の前提条件として同じフェーズで直した。seek は
`AV_TIME_BASE` のマイクロ秒へ変換され、チャンクは要求サンプル位置まで
トリムされる。

### 音声の準備と停止（解決済み、フェーズ A4）

A3 は「鳴っている音が正しい時刻か」を直したが、実機で残ったのは
**鳴り始めるまでと、鳴り止むとき**だった。

[HIGH-23](closed/HIGH-23-resampled-audio-not-cached.md)（リサンプル結果が
未キャッシュ）は、レイヤーを 1 フレーム動かすだけで全長リサンプルをやり直す。
その間は初回なら無音、再送なら**古い配置のまま鳴り続ける**ので、
`start_frame` / `in_frame` / `out_frame` の編集が効かないように見える。
debug ビルドでは音声 1 秒あたり 0.3 秒かかるため（release の約 92 倍）、
開発中は 4 分の曲で 74 秒の無音になる。

[HIGH-24](closed/HIGH-24-timeline-end-pause-not-forwarded-to-audio.md)（終端の
自動 Pause が転送されない）は、`Transport::tick_with` がフレーム不変の tick を
`None` で捨てるため `forward_transport(false, …)` に到達しない。画は止まり音だけ
鳴り続ける。

2 件は無関係な箇所だが、**同じ「音声レイヤーを 1 本置いて再生する」操作で
どちらも踏む**ので 1 フェーズにまとめてある（フェーズ A4）。
`MED-AUD-02`（上限超過が無言）と `MED-AUD-03`（準備中が UI に出ない）は
HIGH-23 の待ち時間をユーザーに説明する側の話なので同じフェーズ。

---

## 構造的負債（コード量あたりの影響が大きいもの）

- **未使用のサブシステムが設計を縛っている**: クラッシュ復旧ジャーナル、`GraphMutation`、
  スレッドプール群（`EvalPool` / `DecodePool` / チャネル / `io_runtime`）はすべて
  呼び出し元ゼロ。しかも bincode のフィールドレイアウト制約が `graph.rs` 全体の
  設計コメントを縛り、フォーマットバージョンは既に5回上がっている。
  さらにジャーナルの粒度（フラットグラフ操作）は実際の undo 単位（`Document`）を覆えない。
  → 昇格させるか削除するかを決める判断が必要。
  [medium/core-evaluator.md](medium/core-evaluator.md) MED-CORE-08
- **設定レイヤー全体が dead**: `settings.toml` は書かれるが `resolved_settings` の
  消費側が無い。結果、完全にメンテされている `ja.toml`（235キー）を
  ユーザー操作で有効化する手段が無い。
  [medium/app-shell.md](medium/app-shell.md) MED-APP-10
  → `SET-1` で解決済み（PR #276）。`settings.toml` の `locale = "ja"` が UI に効き、
  `SET-3` / `SET-4`（PR #278）で環境設定から切り替えられるようになった。
  キーバインドのユーザー上書き（旧 `LOW-APP-15`）も `SET-5` で入り、
  [closed/low.md](closed/low.md) へ移した。
- **宣言済みで実装に繋がっていない殻のフィールド**:
  `Composition.background_color` の未配線は PR #213 で解決済み。
  `Layer` 側では `track_matte` /
  `time_remap` が評価に現れず、逆に `parent` は評価だけ効いて設定 UI が無い。
  → 残る 3 つは `docs/implementation/layer-shell-wiring-plan.md`（`parent` は
  単位 5）が引き受ける
- **手動同期の重複**: Timeline の行レイアウト走査が4箇所、パネル間で
  バイト単位同一のヘルパーが複数、ノードエディタが自前の `NodeRegistry` を持つ。
  [medium/app-shell.md](medium/app-shell.md) MED-APP-13 / MED-APP-14、
  [low/backlog.md](low/backlog.md) LOW-APP-12

---

## 将来のクラッシュ / 誤動作の芽

**この節に挙げていた 5 件のうち 4 件が解決済み**（個票は
[closed/medium-core-evaluator.md](closed/medium-core-evaluator.md) /
[closed/medium-gpu-nodes.md](closed/medium-gpu-nodes.md) /
[closed/medium-media-audio.md](closed/medium-media-audio.md)）。

- `MED-CORE-04`（評価とサブネット走査の深さ上限なし）→ **一部のみ解決。**
  評価の再帰には `EvalError::DepthLimitExceeded`、ロード後には
  `validate_subnet_depth` が入り、**デシリアライズ経路は `HIGH-26` が閉じた**
  （RON リーダは全経路が `RON_RECURSION_LIMIT` で、予算超過はパース中に
  エラーを返す）。残るのは評価側の再帰を明示スタックに変える分で、
  [medium/core-evaluator.md](medium/core-evaluator.md) に置いてある
- `MED-CORE-03`（キャッシュ有効判定が `ctx.time` を無視）→ `CACHE-2` の `CacheIdentity`
- `MED-GPU-03`（ブラー半径が未クランプ）→ `MAX_BLUR_RADIUS` でクランプ
- `MED-MED-04` / `MED-MED-05`（音声エンコーダのチャンネルレイアウトとフレームサイズ）
  → `ChannelLayout::default_for_channels` と `audio_pending` バッファ。
  **書き出しを作る前に踏み終えた**

残る latent な項目は `MED-CORE-04` の評価側の再帰と、
[low/backlog.md](low/backlog.md) の `LOW-APP-16`（Timeline の壊れやすい
panic 箇所）、そして下の `HIGH-33`。

`HIGH-34`（オフラインの素材が黙って透明になる）と `HIGH-35`（参照 ID の
パラメータをフレームごとに動かせる）は**解決済み** — `WARN-1` / `WARN-2` が
1 つの設計で閉じた（個票は
[closed/HIGH-34-offline-media-renders-silently-transparent.md](closed/HIGH-34-offline-media-renders-silently-transparent.md) /
[closed/HIGH-35-identifier-parameters-can-be-driven-by-a-wire.md](closed/HIGH-35-identifier-parameters-can-be-driven-by-a-wire.md)）。

- **[HIGH-33](high/HIGH-33-no-gpu-device-loss-recovery.md)**
  GPU デバイス喪失から復帰できない。`ravel-gpu` に復旧経路が無く、`ZC-8` で
  ウィンドウのレンダラとデバイスを共有するようになったため、GPUI が新しい
  デバイスで復旧しても Ravel は死んだデバイスを持ち続ける。クロスデバイス
  描画だけは `ZC-8` が塞いだが、**復帰後に GPU 評価が動かないことは未解決**
