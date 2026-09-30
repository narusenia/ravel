# 結果の前提照合計画（フェーズ A6 先頭エピック）

> **Status**: 計画のみ — 2026-09-30。単位 `PROV-1`〜`PROV-6` は未着手

## 背景

`roadmap.md` フェーズ A6 の先頭 4 件は、場所は別々だが欠陥は 1 つ —
**返ってきた結果にもキャッシュ行にも「どの前提で作ったか」の識別子が無い**
（または有っても受け入れる地点で照合していない）。

| issue | 受け入れ地点 | 抜けている前提 | 症状 |
|---|---|---|---|
| `MED-APP-37` | Viewer への publish（`ProjectState::on_eval_update`） | どのコンプを評価したか | コンプ切替中に届いた A の絵を B の寸法で解釈する。オーバーレイがずれ、ピクセル読み取りが別の画素を報告する |
| `MED-APP-38` | 出力段フレームキャッシュへの insert（ワーカー） | 表示設定（チャンネル / ピクセル読み取り） | UI スレッドの `clear()` より前に古い設定で finalize した評価が、`clear()` の**後に** insert し、次の無効化まで残る |
| `MED-APP-39` | Timeline のキャッシュ帯の再計算（`publish_cache_band`） | 帯を計算した係数・コンプ | `Full → 1/2 → Full` の往復で version が動かず、帯が別の係数のまま残る |
| `MED-MED-08` | 共有デコードキャッシュのヒット（`ravel-media` `FrameKey`） | 素材ファイルの版 | 同じパスへの上書き後も古いデコード結果を返し続ける |

2026-09-30 に 4 件とも main（`0b29c474`）で未修正であることを現物で確認した。
個票の行番号は古い。以下は同日の実測。

- `crates/ravel-core/src/runtime/eval_service.rs`: `EvalUpdate`（:209）は
  `generation` / `frame` / `results` / `scoped` / `timings` だけでコンプを持たない。
  `EvalRequest::comp`（:266）は要求とキャッシュ行 `(comp, CacheIdentity)` には
  載る（`frames.get` :919、`frames.insert` :952）。insert は finalize の直後に
  **無条件**。`cancel_pending`（:1137）/ `latest_generation`（:1146）が既存の fence
- `crates/ravel-core/src/runtime/frame_cache.rs`: `version`（:178）は insert と
  drop でしか動かない。`clear()`（:398 / :837）は空のキャッシュでは version を
  動かさない。`invalidate_comp`（:832）と `clear` が UI スレッドから呼べる口
- `crates/ravel-app/src/project_state.rs`: `published_generation`（:396）の
  単調受け入れだけが publish の門（:2536）。`composition_resolution` は
  「今アクティブなコンプ」を読む（:2553-2566）。`set_active_composition`（:1167）
  は fence しない。`set_display_channel`（:2176、clear :2191）/
  `set_pixel_readout`（:2215、clear :2221）は atomic を store → `clear()` →
  再要求。`publish_cache_band`（:2619）は version 一致で早期 return（:2628）。
  `published_band_version`（:399）は `set_viewer_resolution`（:2141）でも、
  `VRES-4` の自動降格の解除（`note_viewer_interaction` :2098 →
  `effective_viewer_resolution` :2061）でも、コンプ切替でも落ちない
- `crates/ravel-media/src/frame_cache.rs`: `FrameKey`（:84）は
  `(path, color_space, stream_index, frame)`。`crates/ravel-nodes/src/media.rs`
  の `OpenReader` 比較（:197）も path と色空間だけで、上書き後も古いハンドルを
  読み続ける

`MED-APP-39` の帯は係数の数だけ漏れている。個票が挙げた `set_viewer_resolution`
に加え、`VRES-4` の降格解除とコンプ切替も同じ穴を通る（どちらも setter を
通らないので、setter で `clear_cache_band` を呼ぶ修正では塞がらない）。

## 目的

- 結果を受け入れる 4 つの地点すべてで、**その結果が作られた前提と今の前提を
  照合し、一致しなければ受け入れない**
- 前提が変わったことを受け入れ側が知らなくても安全側に倒れる形にする
  （「変更する側が忘れずに消す」に依存しない）
- 新しい機構は最小にする。既存の世代番号・version を使い回し、足りない 2 つ
  （キャッシュ insert の epoch、素材の版）だけを足す

## 目標アーキテクチャ

### 規則は 1 つ、識別子は受け入れ地点ごと

> 結果を受け入れる地点（publish / insert / 帯の再計算 / デコードのヒット）は、
> その結果が作られたときの前提の識別子を、**受け入れる瞬間の**現在値と比べる。
> 比べられないもの・一致しないものは受け入れない。

| 受け入れ地点 | 識別子 | 新規か | 前提を変える側がすること |
|---|---|---|---|
| Viewer publish | 評価の世代（`generation`）と fence（`published_generation`） | 既存 | 結果を**誤り**にする変更（コンプ切替、表示チャンネル、ピクセル読み取り）は再要求の前に `published_generation = latest_generation()` で fence する |
| フレームキャッシュ insert | キャッシュの insert epoch（ワーカーが要求の取り出し時に受け取る ticket） | **新規** | `SharedFrameCache::clear` / `invalidate_comp` が epoch を進める（空でも進める） |
| キャッシュ帯 | 帯の入力一式 `(frame cache version, comp, 要求文脈)` | version は既存、鍵を拡張 | 何もしない。入力が変われば鍵が変わる |
| デコードのヒット | 素材の版 `content_revision`（`MediaAssetEntry`、セッション限り） | **新規** | ファイル監視が変更を検知したら `DocumentStore::rederive` で版を進める |

### 単一の epoch ではなく地点ごとの識別子にする理由

- **4 つの地点は 3 クレートに分かれ、うち 2 つは既に識別子を持っている。**
  プロセス全体で 1 つの epoch にすると、それを全前提の変更で進める必要があり、
  進めるたびに**全地点**が捨てる（素材 1 本の上書きで無関係なコンプの Viewer
  結果まで捨て、表示チャンネルの切替でデコードキャッシュまで捨てる）
- `ravel-media` は app の状態を見られない。素材の版は Document に載せて
  評価経路で運ぶしかなく、それは他の前提とは運び方が違う
- 規則を 1 つにしておけば、地点ごとの識別子でもレビューの観点は 1 つで済む

### 誤りの非対称性

**有効な結果を捨てたときの損失は再評価 1 回**、古い結果を受け入れたときの損失は
**次の無効化まで誤った絵が残る**（`MED-APP-38` / `MED-MED-08` は一過性ではない）。
だから照合はすべて「一致しなければ捨てる」に倒し、識別子は**気前よく進める**:

- `clear()` は空のキャッシュでも epoch を進める（insert 待ちの評価が
  居るかどうかを UI スレッドは知らない）
- 素材の変更通知は内容が同じでも版を進める（mtime / size の比較もしない）
- fence は「結果が誤りになる」変更で打つ。**精度が落ちるだけの変更**
  （プレビュー解像度）は fence しない — 飛んでいる旧係数の結果は正しい絵で、
  捨てると `VRES-4` が狙う応答性を失う

### 個票の修正案から変えた点

| issue | 個票の案 | この計画 | 理由 |
|---|---|---|---|
| `MED-APP-37` | `EvalUpdate` に comp id を載せて照合 | 切替時に既存の fence を打つ | ravel-core の API を変えずに済み、表示設定の setter にも同じ手が効く。世代は要求順に単調なので、fence 以前の世代はすべて切替前の要求 |
| `MED-APP-38` | 設定の世代を要求と結果に載せる | キャッシュ側の insert epoch | 要求型と `ReadAheadTemplate` に設定の概念を通さずに済み、UI スレッドからの**あらゆる**無効化（将来の `invalidate_comp` 呼び出しを含む）を守る。フェーズ A3 の `TransportSync::try_commit_frames`（`ravel-audio/src/sync.rs:60`、期待 epoch と一致したときだけ commit）と同じ形 |
| `MED-APP-39` | `set_viewer_resolution` で `clear_cache_band` | 帯の鍵を入力一式にする | 降格解除とコンプ切替には呼べる setter が無い |
| `MED-MED-08` | インポート時の mtime+size を版にする / 監視 | 監視 + セッション限りの版 | インポート時の値だけでは開いている間の上書きを検知できない。版を Document に置くと、既存の文書 diff（`frame_cache.rs:446`、`eval.rs:2123`）がフレームキャッシュとノードキャッシュの無効化をそのまま引き受ける |

### insert epoch の順序の不変条件（`PROV-3`）

表示設定は atomic のままワーカーの `finalize` が読む。正しさは次の順序で決まる:

1. setter は**設定の store → `clear()`** の順に呼ぶ（現状どおり。入れ替えると壊れる）
2. ワーカーは**要求の取り出し時（`sync_document` の後、評価の前）に ticket を取り**、
   その後で `finalize` が設定を読む
3. `insert` はキャッシュのロック下で ticket と現在の epoch を比べる

ticket が bump より前 → insert が拒否される。後 → ミューテックスの
happens-before により finalize は新しい設定を読んでいる。どちらでも古い設定の
絵はキャッシュに残らない。先読み（speculative）も同じ経路を通る。

## Phase 1: Viewer と帯の受け入れ（ravel-app のみ）

### 主な対象

- `crates/ravel-app/src/project_state.rs`

### 作業

- `PROV-1`: `set_active_composition` / `set_display_channel` /
  `set_pixel_readout` が再要求の前に `published_generation` を
  `latest_generation()` へ進める。`published_generation` の doc コメントに
  「結果を誤りにする前提の変更は fence する。精度だけの変更はしない」を書く。
  `composition_resolution` をアクティブなコンプから読む箇所は、fence により
  「publish される結果は切替後の要求のもの」が保証されるので残してよい
- `PROV-2`: `published_band_version: Option<u64>` を、帯の入力一式
  （frame cache version、コンプ id、`viewer_eval_context(comp, 0)` の
  キャッシュ照合に効く軸）を持つ鍵に置き換え、`publish_cache_band` の早期
  return をその一致で判定する

### 完了条件

- `PROV-1`: コンプ切替の後に切替前の世代の `ViewerUpdate` を配達しても
  `ViewerFrame` と `EvalResults` が変わらないテストがある。表示チャンネルと
  ピクセル読み取りの切替でも同じ。fence を消すと落ちることを確認済み
- `PROV-2`: 2 つの係数のフレームを両方キャッシュに入れてから係数を戻すと、帯が
  戻した係数のものになるテストがある。`VRES-4` の降格解除と、キャッシュヒットで
  終わるコンプ切替でも帯が再計算されるテストがある
- `MED-APP-37`（`PROV-1`）/ `MED-APP-39`（`PROV-2`）を `issues/closed/` へ移し、
  `**解決済み**` 行を付ける

## Phase 2: フレームキャッシュ insert の epoch（ravel-core）

### 主な対象

- `crates/ravel-core/src/runtime/frame_cache.rs`
- `crates/ravel-core/src/runtime/eval_service.rs`

### 作業

- `PROV-3`: `FrameCache` に insert epoch を持たせ、公開の `clear` /
  `invalidate_comp` が（空でも）進める。ワーカーは要求の取り出し時に ticket を
  取り、`insert` に渡す。ticket が古い insert は捨てる（予算の予約もしない）。
  捨てた回数を `FrameCacheStats` に数え、`eval result sent` のログに載せる
  （「キャッシュが効かない」を観測できるように — `CACHE-6` の方針）。
  順序の不変条件（上節）をコードのコメントに書く

### 完了条件

- ticket を取る → `clear()` → 古い ticket で insert → エントリが無い、の単体テスト。
  空のキャッシュへの `clear()` でも同じ結果になるテスト
- ワーカー経由の決定的なテスト: finalize で止まるフック（テストがチャンネルで
  解放する）を使い、評価中に `clear()` を呼んでから解放して、キャッシュに
  古い設定のエントリが入らないことを確かめる。先読みの要求でも同じ
- epoch の比較を外すとワーカー経由のテストが落ちることを確認済み
- `MED-APP-38` を `issues/closed/` へ移す

## Phase 3: 素材の版（ravel-core / ravel-nodes / ravel-media / ravel-app）

### 主な対象

- `crates/ravel-core/src/composition/asset.rs`（`MediaAssetEntry`）
- `crates/ravel-media/src/frame_cache.rs`（`FrameKey`）
- `crates/ravel-nodes/src/media.rs`（`OpenReader`、キーの組み立て :217 / :236）
- `crates/ravel-app/src/project_state.rs`、`crates/ravel-app/src/media/`

### 作業

- `PROV-4`（ヘッドレス）: `MediaAssetEntry` に `#[serde(skip)]` の
  `content_revision: u64` を足す（`resolved` と同じくセッション限り、形式の版は
  上げない）。`FrameKey` に版を足し、`OpenReader` も版が変われば開き直す。
  版だけが違う Document の diff がフレームキャッシュとノードキャッシュを
  無効化することをテストで固定する（既存の `media_assets` 比較が拾う）
- `PROV-5`（app）: 解決済みパスの親ディレクトリを `notify` で監視し
  （前例: `themes.rs:238` の `ThemeWatch`。新しい依存は足さない）、変更された
  パスに一致する素材の版を `DocumentStore::rederive` で進めて再要求する。
  rederive は undo 段を作らず、プロジェクトを dirty にしない（前例:
  `rebase_asset_references` :1501）。書き込み途中の連続イベントはまとめる。
  版を進める判定は純粋関数（Document と変更パスの集合 → Document）に出して
  テストする

### 完了条件

- `PROV-4`: 同じパスの中身を差し替えて版を進めると新しいピクセルが返るテスト
  （デコード回数を数えるスタブで、キャッシュを経由したことも確かめる）。
  リーダーが開き直されるテスト。版だけが違う diff でフレームキャッシュの
  エントリが消えるテスト
- `PROV-5`: 版を進める純粋関数のテスト（一致する素材だけが進み、連番は
  ディレクトリ単位で一致する）。`ProjectState` 経由で undo 段が増えず dirty に
  ならず、再要求が出るテスト。監視そのものは手動確認（外部から素材を
  上書きして Viewer が追随する）を PR に記す
- `MED-MED-08` を `issues/closed/` へ移す（`PROV-5`）

## Phase 4: 文書

- `PROV-6`: `docs/specifications/architecture.md` の評価・キャッシュの節に
  「受け入れ地点で前提を照合する」規則と 4 地点の表を書く。`cache-plan.md` の
  `CACHE-5` / `CACHE-6` / `CACHE-8` の記述から本計画へ参照を張る。
  `docs/dev/doc-checklist.md` を辿る。計画を `done/` へ移す

### 完了条件

- `mise run docs:check` が通る。4 件の issue がすべて closed にある

## テスト方針

- すべてヘッドレス（`gpui::test` か ravel-core / ravel-nodes の単体テスト）で書ける。
  GPU は要らない。監視（`PROV-5`）だけが手動確認を残す
- 各単位は**照合を外すと落ちる**ことを実装者と独立検証の両方で確かめる
  （変異検査）。受け入れを検査するテストは、捨てる側と通す側の両方を持つ
  （常に捨てる実装でも通るテストにしない）

## 非対象

- A6 の残り（`MED-GPU-09`、`MED-CORE-12`、`MED-MED-09`、`MED-MED-06`）。
  別の欠陥なので個別の単位で直す
- コンプ切替から新しい結果が届くまでの間、Viewer に切替前の最終フレームが
  残ること（現状と同じ。fence はこれを変えない）
- 素材の版のフレーム毎 `stat`（個票どおり採らない）とディスクキャッシュ層（`CACHE-11`）
- ravel-cli の素材監視（ヘッドレスのレンダーは開始時点のファイルを読む）

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| `PROV-1` | 前提を誤らせる変更で Viewer の publish を fence（`MED-APP-37`） | — |
| `PROV-2` | キャッシュ帯の鍵を入力一式にする（`MED-APP-39`） | — |
| `PROV-3` | フレームキャッシュの insert epoch（`MED-APP-38`） | — |
| `PROV-4` | 素材の版と `FrameKey` / リーダーへの反映（ヘッドレス） | — |
| `PROV-5` | 素材の監視と版の更新（`MED-MED-08`） | `PROV-4` |
| `PROV-6` | 規則の仕様化と計画のクローズ | `PROV-1`〜`5` |

`PROV-1`〜`PROV-4` は互いに独立で並行できる。触るファイルは `PROV-1` と
`PROV-2` が同じ `project_state.rs`（別の関数）なので、並行するならどちらかを
先にマージしてからリベースする。

## 判断待ち

- **素材の変更の検知方法**（`PROV-5`）: ファイル監視（推奨。Ravel 自身の
  レンダーが素材パスへ書いた場合も拾う）/ 手動の「素材を再読み込み」コマンド /
  アプリのフォーカス復帰時の `stat`
- **素材の変更でノードキャッシュを全部捨てること**: 既存の
  `media_assets` 比較（`eval.rs:2123`）は構造変更として扱い、ノードキャッシュを
  すべて捨てる（`invalidate_all`）。フレームキャッシュも同様（`frame_cache.rs:446`）。
  上書きは稀なので許容する想定だが、素材単位に絞るなら `PROV-4` の範囲が広がる

## 実施状況

未着手。
