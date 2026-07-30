# Ravel — 課題インデックス

コードベース全体（8クレート、約9.1万行）を対象に、技術的負債・パフォーマンス問題・
バグを網羅調査した結果。**全項目はソース上で実物を確認済み**（仮説・スタイル指摘は除外）。

調査範囲: `ravel-core` / `ravel-nodes` / `ravel-gpu` / `ravel-media` / `ravel-audio` /
`ravel-i18n` / `ravel-ui` / `ravel-app`、および `assets` のロケールデータ。

| 深刻度 | 件数 | 場所 |
| --- | --- | --- |
| critical | 4（1件解決） | [critical/](critical/) — 1件1ファイル |
| high | 24（5件解決） | [high/](high/) — 1件1ファイル |
| medium | 49（3件解決） | [medium/](medium/) — 領域別5ファイル |
| low | 31（1件解決） | [low/backlog.md](low/backlog.md) — 1ファイル |

解決済みの項目は該当ファイル冒頭に PR 番号付きで記載する（行は消さない）。

> **着手順の正は `docs/implementation/roadmap.md`。** この文書は「何が壊れて
> いるか」と「どの項目が互いに絡んでいるか」の台帳で、**順序を決めない**。
> ロードマップは issue をクラスタ単位でフェーズに割り当てており、下記の
> クラスタ名がそのまま対応する。個票（原因・該当行・修正方針）はここにある。

| クラスタ | ロードマップ上の位置 |
|---|---|
| データ保全 + 無言の失敗 + latent クラッシュ | フェーズ A2「失われないこと」 |
| 音声・A/V 同期 | フェーズ A3「音声の正しさ」 |
| 音声の準備と停止（実機で発覚） | フェーズ A4「音声が編集に追いつくこと」 |
| もっさり 第1段（評価・レンダー回数） | 完了（`done/ui-responsiveness-plan.md`） |
| もっさり 第2段（描画1回あたり） | フェーズ A / H（`gpu-compositing-plan.md`） |
| もっさり 第3段（評価器のアルゴリズム）+ パネル1回あたり | フェーズ C3「応答性の残り」 |
| もっさり 第4段（メディア・スクラブ） | フェーズ C2（`cache-plan.md`）と C3 |
| 設定が効かない / キーバインド上書き | フェーズ C（`settings-screen-plan.md`） |
| 操作の正しさ（Timeline / NodeEditor / 選択） | フェーズ E |
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

1. **[CRIT-01](critical/CRIT-01-eval-update-notifies-whole-workspace.md)**
   評価結果ごとに全5パネルがモデル再構築 + 再レンダー。再生中は毎フレーム。
   これが他のすべてのコストに掛かる倍率になっている。実質1箇所の修正。
2. **[HIGH-07](high/HIGH-07-document-changed-cascade-per-mouse-move.md)**
   マウス移動ごとに `document_changed` の全カスケード（選択プルーン、音声同期、
   コンパイル破棄、5パネル notify、選択グローバル再 publish の第2波）。
3. **[HIGH-06](high/HIGH-06-pipeline-recompiled-per-param-edit.md)**
   スライダードラッグ中に GPU コンピュートパイプラインを毎回再コンパイル（naga 再検証込み）。
   ただし実測では tick の約23%で、「編集中の重さ」の主因ではない（第2段が主因）。
   詳細は `docs/implementation/perf-baseline.md`「RESP-3 完了時」。

### 第2段: 描画1回あたりのコストを削る

**実測上の主因はここ**。設計と実装単位は
`docs/implementation/gpu-compositing-plan.md`（GPUCOMP-1〜11）。

4. **[HIGH-05](high/HIGH-05-shell-chain-cpu-per-pixel.md)** — 解決済み（2026-07-29）
   シェル合成チェーン（transform / opacity / merge）が CPU per-pixel のため、
   レイヤーごとにブロッキング GPU リードバックを強制していた。GPUCOMP-2/3/5/6 で
   3プロセッサすべてに GPU 版が入り、シェルチェーン由来のリードバックは 0 になった。
   チェーン全体の回帰 pin は GPUCOMP-7。
5. **[HIGH-04](high/HIGH-04-per-frame-blocking-readback.md)**
   リードバックそのものが最悪実装（毎回ステージング確保 + デバイス全体待ち + 二重コピー）。
6. **[HIGH-08](high/HIGH-08-ui-thread-f32-to-bgra-conversion.md)** /
   **[HIGH-09](high/HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md)**
   UI スレッドでの全フレーム色変換と GPU→CPU→GPU 往復 + アトラス churn。

### 第3段: 評価器のアルゴリズム的コスト

7. **[HIGH-01](high/HIGH-01-evaluator-no-adjacency-index.md)**
   隣接インデックスが無く、ノード訪問ごとに全エッジ走査（1回の pull が O(N·E)）。
8. **[HIGH-02](high/HIGH-02-graph-eq-no-ptr-eq-fastpath.md)**
   編集ごとに全レイヤーネットワークを deep compare（`Arc::ptr_eq` の高速路が無い）。
9. **[HIGH-03](high/HIGH-03-params-resolved-per-visit.md)**
   キャッシュヒット時でもパラメータ全再解決、`PathPoints` を毎フレーム clone。
   → `docs/implementation/cache-plan.md` の CACHE-2 が引き受ける。

### 第4段: メディア・スクラブ

10. **[HIGH-16](high/HIGH-16-no-decoded-frame-cache.md)**
    デコード済みフレームキャッシュが無く、逆方向スクラブと再描画で GOP を丸ごと再デコード。
    → `docs/implementation/cache-plan.md` の CACHE-8 が引き受ける。
11. **[HIGH-17](high/HIGH-17-sws-scaler-recreated-per-frame.md)**
    sws スケーラをフレームごとに再生成 + スカラー per-pixel 変換。

### 独立: NodeEditor 固有の再描画（第1段の効果を打ち消している）

フェーズ A で潰す（`HIGH-21` / `HIGH-22`）。

12. **[HIGH-21](high/HIGH-21-node-editor-repaints-every-playback-frame.md)**
    第1段（RESP-1〜3）は「評価結果でパネルを notify しない」方針に切り替えたが、
    NodeEditor は評価結果を運ぶグローバル（`NodeEvalTimings`）を**無条件に
    observe して notify** しており、再生中は毎フレーム全再構築される。
    しかも `add_node_menu_model` の再構築が `no_network` 分岐より手前にあるため
    **ネットワークを閉じていても毎フレーム走る**。第1段・第2段とは独立した原因で、
    このパネルだけが第1段の効果を受けていない。

### 補足

パネル側の1回あたりコスト（Timeline の行仮想化欠如、Properties のフレーム2回再構築、
Outliner の全走査、コンポジションの毎編集再コンパイル、
同じ変更が2経路から届く重複 sync）は
[medium/ui-rendering.md](medium/ui-rendering.md) にまとめてある。
第1段を直すと呼ばれる回数は減るが、レイヤー数が増えるとこれらが再び効いてくる。

---

## データ保全（もっさりとは独立に優先度が高い）

- **[CRIT-02](critical/CRIT-02-save-failure-invisible-and-swallows-quit.md)**
  保存失敗が完全に不可視。しかもガード付き保存の失敗で Quit / Close が無言で破棄される
- **[CRIT-03](critical/CRIT-03-project-write-not-atomic.md)**
  保存が truncate → write の非アトミック。クラッシュで `.ravprj` が破損し、
  `.bak` へのフォールバック経路も無い
- **[CRIT-04](critical/CRIT-04-uncommitted-gesture-baked-by-foreign-commit.md)**
  ペン / ドラッグの未コミット状態が他パネルのコミットで焼き込まれ、Esc が無効化される
- オートセーブとクラッシュ復旧ジャーナルはどちらも未配線
  → [medium/app-shell.md](medium/app-shell.md) MED-APP-10 / MED-APP-11、
  [medium/core-evaluator.md](medium/core-evaluator.md) MED-CORE-08

保存失敗が見えず・保存自体が非アトミック・オートセーブもジャーナルも無いという3点が
同時に成立しているので、この4件は独立した1エピックとして扱うのが妥当。
**フェーズ A2「失われないこと」がこのエピック**で、無言の失敗（`HIGH-18` /
`HIGH-20` / `MED-APP-12`）と latent クラッシュ（`MED-CORE-04` / `MED-GPU-03` /
`LOW-GPU-01`）も同じフェーズに入る。

---

## 音声・A/V 同期（まとめて設計を見直すべき塊）

[HIGH-12](high/HIGH-12-pause-does-not-stop-queued-audio.md)（Pause でキューが止まらない）、
[HIGH-13](high/HIGH-13-seek-does-not-flush-audio-queue.md)（Seek で flush しない）、
[HIGH-14](high/HIGH-14-clock-advances-over-underrun.md)（アンダーラン中もクロック進行）、
[HIGH-15](high/HIGH-15-settrack-resamples-on-prep-thread.md)（SetTrack が prep スレッドをブロック）
は互いに増幅し合う。SetTrack のブロックがアンダーランを起こし、
アンダーランがクロックドリフトになり、Pause / Seek がそれぞれ固定オフセットを追加する。
個別に直すより、チャンクキューに epoch を導入する設計変更で4件同時に解ける。
**フェーズ A3「音声の正しさ」**がこの塊で、`MED-MED-03/04/05` と `MED-AUD-01`
も同じフェーズに入る。

音声デコーダ側の [HIGH-10](high/HIGH-10-audio-chunk-seek-wrong-time-base.md) /
[HIGH-11](high/HIGH-11-audio-chunk-no-trim.md) は、
ストリーミング音声再生を実装する前に直しておくべき前提条件。
片方はミックスダウンの上限プローブで既に実害が出ている。

### 音声の準備と停止（解決済み、フェーズ A4）

A3 は「鳴っている音が正しい時刻か」を直したが、実機で残ったのは
**鳴り始めるまでと、鳴り止むとき**だった。

[HIGH-23](high/HIGH-23-resampled-audio-not-cached.md)（リサンプル結果が
未キャッシュ）は、レイヤーを 1 フレーム動かすだけで全長リサンプルをやり直す。
その間は初回なら無音、再送なら**古い配置のまま鳴り続ける**ので、
`start_frame` / `in_frame` / `out_frame` の編集が効かないように見える。
debug ビルドでは音声 1 秒あたり 0.3 秒かかるため（release の約 92 倍）、
開発中は 4 分の曲で 74 秒の無音になる。

[HIGH-24](high/HIGH-24-timeline-end-pause-not-forwarded-to-audio.md)（終端の
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
  → `docs/implementation/settings-screen-plan.md` の SET-1 が引き受ける
  （UI より先に「効く」を作る単位）。キーバインドのユーザー上書き
  （[low/backlog.md](low/backlog.md) LOW-APP-15）は SET-5。
- **宣言済みで実装に繋がっていない殻 / コンプのフィールド**:
  `Composition.background_color` は保存も編集もできるのに評価されない
  （[medium/core-evaluator.md](medium/core-evaluator.md) MED-CORE-09。Viewer は
  黒 quad をハードコードしている）。`Layer` 側では `track_matte` /
  `time_remap` が評価に現れず、逆に `parent` は評価だけ効いて設定 UI が無い。
  → 前者は `docs/implementation/viewer-inspection-plan.md` の INSP-1、
  後者 3 つは `docs/implementation/layer-shell-wiring-plan.md`（`parent` は
  単位 5）が引き受ける
- **`GpuTask` バッチング trait の実装がゼロ**: doc コメントは
  「フレームあたり1コマンドバッファにバッチする」と約束するが、
  実際はノードごとに submit。[medium/gpu-nodes.md](medium/gpu-nodes.md) MED-GPU-01
- **手動同期の重複**: Timeline の行レイアウト走査が4箇所、パネル間で
  バイト単位同一のヘルパーが複数、ノードエディタが自前の `NodeRegistry` を持つ。
  [medium/app-shell.md](medium/app-shell.md) MED-APP-13 / MED-APP-14、
  [low/backlog.md](low/backlog.md) LOW-APP-12

---

## 将来のクラッシュ / 誤動作の芽（現状 latent）

- **[MED-CORE-04](medium/core-evaluator.md)** 評価とサブネット走査に深さ上限が無い。
  深いチェーンでワーカースレッドがスタックオーバーフローし、catch 不能に abort する。
  細工 / 破損した `.ravprj` でロード時クラッシュも可能
- **[MED-CORE-03](medium/core-evaluator.md)** キャッシュ有効判定が `ctx.time` を無視。
  モーションブラー / タイムリマップを実装した瞬間「N サンプルが全部同一」になる。
  → `docs/implementation/cache-plan.md` の CACHE-2 が引き受ける（旧 BLUR-2 も統合）
- **[MED-GPU-03](medium/gpu-nodes.md)** ブラー半径が未クランプ。大きな値で GPU が TDR / ハング
- **[MED-MED-04](medium/media-audio.md)** / **[MED-MED-05](medium/media-audio.md)**
  音声エンコーダのチャンネルレイアウトとフレームサイズ処理。
  エクスポート機能を作った時点で必ず踏む
