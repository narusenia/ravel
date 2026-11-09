# ステートフル評価と sim キャッシュ実装計画（REQ-CORE-011）

> **Status**: In progress — 単位 1〜4 は #639 でマージ（2026-11-10）。単位 5 が残る

対象要件: REQ-CORE-011（優先度 Must）。関連: REQ-CORE-002（Hybrid Pull）、
REQ-CORE-005（バックグラウンド評価）、REQ-CORE-006（三層キャッシュ）。

`particle-plan.md`（REQ-MOGRAPH-002）の前提。パーティクルが最初の
利用者だが、トレイル・物理・時間積分系すべてが同じ機構に乗るため、
評価エンジンの改修として独立させる（AGENTS.md の design gate が
「evaluation」のサブシステム改修を明示的に挙げている）。

**前提**: `evaluation-scope-plan.md` の単位 1（`PathSegment` の
スコープ次元）。本計画・FX-5・グラフ内反復が同じキャッシュ制約に
当たっており、軸を先に共通化しないと機構が 3 つに分裂する。

設計は `docs/specifications/procedural-geometry.md:88-127` に既にある。
本計画はそれを実装単位へ落とすもので、**設計を変更しない**。

## 問題

Hybrid Pull（REQ-CORE-002）は「フレーム t の値は t だけから決まる」前提で
組まれている。`Evaluator` のキャッシュ（`eval.rs:457`）は
`NodeKey`（path + node）から `CacheEntry` への map で、フレームが変われば
再計算するだけ。**前フレームの結果に依存するノードを表現できない。**

`procedural-geometry.md` の「既存コードへの影響」表でも
`ravel-core/src/eval.rs` の sim キャッシュ統合は 🔲 未着手。

## 決定事項

すべて `procedural-geometry.md` の既存記述に従う。ここでは実装上の
含意だけ補う。

### `StatefulProcessor` は `NodeProcessor` と別トレイト

```rust
pub trait StatefulProcessor {
    type State: Send + Sync;
    fn initial(&self, ctx: &EvalContext, inputs: &Inputs) -> Self::State;
    fn step(&self, prev: &Self::State, ctx: &EvalContext, inputs: &Inputs)
        -> Self::State;
}
```

`Evaluator` に登録する際は型消去したアダプタで包み、
`Arc<dyn NodeData>` を出す通常ノードとして下流から見えるようにする。
下流のノードはステートフルかどうかを知らなくてよい。

### sim キャッシュは別 map。ただしキー型は共有する

既存の `cache: HashMap<NodeKey, CacheEntry>` とは分ける。

```text
sim_cache: HashMap<NodeKey, SimTrack>
SimTrack { start_frame: u64, states: Vec<Arc<dyn Any + Send + Sync>>, input_hash: u64 }
```

フレーム連続区間として持つ。フレーム t の pull で
`[last_cached+1, t]` を順に `step` して埋める。

**`NodeKey` を共有するのが条件**（`evaluation-scope-plan.md` の決定）。
独自のキー型を作らない。sim を汎用キャッシュに載せない理由は
「1 ノードに値が 1 つしか置けないから」ではなく、**フレーム連続区間が
順序に意味を持つ系列で、逐次充填という別のアクセスパターンを持つ**から。
汎用キャッシュに載せるとスクラブのたびに数百エントリが LRU を汚す。

**バイト予算は `cache-plan.md` の `CacheBudget` から受け取る。** sim は
追い出されると再計算が O(フレーム数) で他の層と非対称なので、予算内に
**保護枠**（既定で総額の 25%、設定可変）を持ち、通常のフレームキャッシュの
圧力では削られない。枠を超える長期保持は暗黙 LRU で守るのではなく、
将来の明示キャッシュノードに寄せる（同計画の「将来方向」）。
`SimTrack` 内部の保持は**チェックポイント**で行う: トラックが保護枠
（トラック 1 本あたりの上限）を超えたら、保持する状態を「開始フレーム + 最新
フレーム + `stride` の倍数のフレーム」に間引き、収まるまで `stride` を倍にする。
保持されていないフレームの pull は、直前の保持フレームから `step` で
やり直す（`stride` 未満の step）。規則はフレーム番号と予算だけの関数なので
状態は変わらず、近さだけが変わる。GPU 常駐の sim（PART-6）が VRAM 層で使う
のも同じ規則。

「同一ノードの複数結果を保持できない」という制約そのものは
`evaluation-scope-plan.md` が `PathSegment` の拡張で解く。
本計画・FX-5・グラフ内反復の 3 つが同じ制約に当たっているので、
**回避策を各自で持たない**。

### 無効化は入力ハッシュの全破棄（v1）

上流サブグラフの構造/パラメータハッシュを `SimTrack.input_hash` に記録し、
変化したら**全区間破棄**。キーフレーム変化の影響開始フレーム以降のみ
破棄する最適化（spec の v2）は本計画では行わない。

既存の `Evaluator` は path ベースの scope 無効化を持つ
（`eval.rs:490-511`）。sim キャッシュも同じ経路でまとめて落とす。

### 多段接続は 1 段に制限

spec の制約どおり。sim の下流に sim を繋いだ場合は評価エラーにする
（黙って壊れた結果を出さない）。

### 前方ジャンプは暫定表示

`[last+1, t]` の充填が長い場合、最後のキャッシュ済み状態を返しつつ
バックグラウンドで埋める。既存の `EvalService` は 1 リクエスト =
1 評価なので、**充填の途中経過を返す仕組みが要る**。
`EvalUpdate` に「暫定値」フラグを足す。

なお `done/attribute-spreadsheet-plan.md` の単位 1 が `EvalRequest` /
`EvalUpdate` を複数ターゲット化する。**どちらが先でも構わないが、
両方が同じ型を触る**ので実施順を決めてから着手する。

## 実装単位

### 単位 1: `StatefulProcessor` と sim キャッシュの骨格

- トレイト定義と型消去アダプタ。
- `Evaluator` に `sim_cache` を追加。区間充填ロジック。
- 決定性の担保: `step` は `ctx` と `prev` と入力のみに依存。
  乱数は `seed` パラメータ + `id` 属性由来のハッシュのみ
  （`Date` / `Math.random` 相当の禁止は既存の lint 対象）。

**完了条件**

- カウンタ的なテスト用ステートフルノードで、frame 10 の pull が
  10 回 `step` することを検証。
- frame 10 → frame 5 → frame 10 でキャッシュから即返る（`step` 回数が
  増えない）テスト。
- 同一シード・同一入力で 2 回評価して同一結果になるテスト。
- sim の下流に sim を繋いで評価エラーになるテスト。

### 単位 2: 無効化

- 上流サブグラフの構造/パラメータハッシュ計算。
- パラメータ変更 → 全区間破棄 → 再充填のテスト。
- 既存の scope 無効化経路（`drop_scope_owner_caches`）との統合。

**完了条件**

- パラメータ変更で `SimTrack` が破棄されるテスト。
- **無関係なノードの変更では破棄されない**テスト（過剰無効化の回帰）。
- レイヤーネットワーク内のステートフルノードが、シェル側の編集で
  誤って破棄されないテスト。

### 単位 3: 暫定表示とバックグラウンド充填

- `EvalUpdate` に暫定フラグ。
- 前方ジャンプ時に最後のキャッシュ状態を即返し、充填を継続。
- 充填は評価スレッドプールで行い UI を塞がない（REQ-CORE-005）。

**完了条件**

- frame 0 から frame 300 へジャンプしたとき、最初の `EvalUpdate` が
  暫定フラグ付きで即返るテスト。
- 充填完了後に確定値が publish されるテスト。
- 充填中に別のリクエストが来たときの挙動（充填を捨てる）のテスト。

### 単位 4: 文書更新

- `docs/specifications/procedural-geometry.md`: sim キャッシュ節の
  「未着手」表記を更新。
- `docs/requirements/REQ-CORE.md`: REQ-CORE-011 の受入条件を更新。

### 単位 5: スコープの外から来る入力

#639 のレビューで見つかった 2 つの穴。どちらも「sim が自分のネットワークの
外から値を受け取る」経路で、SIM-1〜4 の前提ハッシュと `sim_inputs_at` は
ネットワークの内側しか見ていない。

1. **束縛が要求フレームで固定される。** `Bindings` は `evaluate_sub` の時点で
   実体化された値（`Vec<(String, Arc<dyn NodeData>)>`）で、
   `evaluate_time_shifted` は「外側の束縛を引き継ぐ」。sim が frame 0..t を
   step するとき、`net.in`（サブネットのピン、調整レイヤーの `source`、
   コンプの入力）は**全 step で要求フレーム t の値**を返す。調整レイヤー上の
   トレイル・フィードバックは sim の主用途なので、これは正しさの問題。
   同じ制限は既存のステートレスな time shift（`time.offset` など）にもある。
2. **前提がスコープの外を見ない。** `layer.ref` の参照先、シェル読み
   （`layer.info` / `comp.info`）、`net.in` の束縛元が変わっても、sim の上流
   サブグラフは変わらないので `discard_if_stale` が古い状態を受け入れる。
   `set_document` の参照元パスや `invalidate_shell_readers` /
   `invalidate_changed_bindings` はノードキャッシュだけを落とす。

**目標の形**

- 束縛は「任意のフレームで引ける源」になる。`TimeShift(node, f)` の下で
  `net.in` が読む束縛は、束縛を作ったスコープでフレーム f に引き直した値。
  sim の充填と replay、およびステートレスな time shift の両方が同じ規則に乗る
  （規則を 2 つにしない）。
- sim の前提は、上流がスコープの外から読むもの（束縛の源の上流、参照先レイヤーの
  ネットワークとシェル）を含む。照合は引く時点で行う（`rust.md` の前提照合の
  規則。誰かが purge を覚えていることに頼らない）。外部を読まない sim では、
  同じネットワーク内の無関係な編集でトラックが落ちない性質（SIM-2）を保つ。

**完了条件**

- サブネットのピンにアニメーションする値を繋ぎ、内側の sim が各 step で
  その step のフレームの値を受け取るテスト（充填と、間引き後の replay の両方）。
- 調整レイヤーの `source` を読む sim が、各 step でそのフレームの下のレイヤーを
  受け取るテスト。
- ステートレスな time shift がサブネットのピンをシフト先のフレームで読むテスト。
- `layer.ref` の参照先レイヤーを編集すると、それを上流に持つ sim のトラックが
  次の pull で破棄されるテスト。シェル読みと束縛元の編集も同様。
- 外部を読まない sim が、同じネットワーク内の無関係な編集で破棄されない
  回帰テスト（SIM-2 のテストが緑のまま）。
- `architecture.md` の前提照合表の sim トラック行を更新。

## 検証

- すべてヘッドレス（`ravel-core`）。GPU 不要。
- **決定性テストを最優先**。sim は非決定性が混ざると再現しないバグの
  温床になるので、同一入力 2 回評価の一致を全単位で回す。
- 過剰無効化はスクラブ体感の劣化として現れるので、単位 2 の
  「無関係な変更で破棄されない」テストを回帰として残す。

## 非対象

- **影響開始フレーム以降のみ破棄する最適化**（spec の v2）。
- **sim キャッシュのディスク層へのスピル**（REQ-CORE-006 の RAM 層まで）。
- **多段のステートフル接続**。
- **GPU でのシミュレーション**。CPU SoA（rayon）のみ
  （`procedural-geometry.md` の GPU 方針に従う）。
- **グラフ内反復**（REQ-CORE-013）。spec が v1 で採用しないと明記。

## 実装メモ

計画の起草（2026-07-27）後にコードが動いたので、計画が引いている位置は
次に読み替える（行番号は腐るので関数名で書く）。

- ノード結果キャッシュ: `eval.rs:457` の `HashMap` ではなく、`eval.rs` の
  `cache_store` モジュールの `CacheStore`（`Evaluator::store`）。
- scope 無効化: `eval.rs:490-511` ではなく `Evaluator::invalidate_scope` /
  `drop_scope_owner_caches` / `prune_scope_state`。`PathSegment` のスコープ
  次元（SCOPE-1）は実装済みで、sim トラックはその `NodeKey` を共有する。
- `EvalRequest` / `EvalUpdate` は `runtime/eval_service.rs`。複数ターゲット化
  （属性スプレッドシート単位 1）は実装済みで、その上に暫定フラグを足す。
- バイト予算: `cache_budget.rs` の `CacheKind::Sim`（保護枠 =
  `sim_reserve_ratio`）が既にあり、sim はそこへ `reserve` する。予算の
  追加実装は無い。

### 単位 1（SIM-1）

- 実装は `crates/ravel-core/src/eval/sim.rs`（`eval.rs` の子モジュール。
  `Evaluator` の private フィールドと `NodeKey` をそのまま使うため）。
- `StatefulProcessor`（`initial` / `step` / `output` / `byte_size`）。計画の
  スケッチに **`output`** を足した。型消去アダプタは状態を `NodeData` へ
  戻す手段が要るため。`SimStepper`（型消去）→ `StatefulAdapter<P>`
  （`NodeProcessor`。`is_time_dependent` と `pulls_inputs_itself` は真）。
  登録は `stateful_processor(p)`（`ProcessorRegistry` からも使える）。
- `SimCache`（`NodeKey → SimTrack`）。`Evaluator::sim_state_at`
  （`EvalScope::sim_state` の実装）が `fill_sim_track` で
  `[last+1, t]` を埋める。1 フレームごとに状態を格納するので、失敗や中断で
  もそこまでの充填は残る。
- 入力とパラメータは**要求フレームではなく各 step のフレーム**で引く
  （`sim_inputs_at`。入力は `TimeShift(node, frame)` スコープ、パラメータは
  `resolve_channel_params` + `materialize_params` + `parameter_overlays`）。
  そうしないと状態が「どのフレームで要求されたか」に依存する。
  `parameter_overlays` は `eval_node` のパラメータポート処理を括り出した
  もので、両者が同じ規則を読む。`pull_input_at` は `evaluate_input_at` の
  本体（`pull_log` へ記録するかを選べる）。sim は記録しない — 記録すると
  キャッシュヒットのたびに全フレーム分の再引きが走る。
- バイト予算: トラック 1 本 = 予約 1 件で、伸びるたびに取り直す。追い出しは
  **トラック単位**（`SimCache::drop_evicted`）で、sim 同士でのみ起こる。
  トラック内の保持はチェックポイント（追補の項）。
- 多段: `Evaluator::sim_active` が立っている間に別の sim が始まれば
  `EvalError::ProcessFailed`（下流に sim を繋いだ）。
- `register` / `invalidate_node` は該当ノードのトラックを落とし、
  `invalidate_all` / `reset` は全部落とす。

### 単位 2（SIM-2）

- 無効化の権威は**引く時点の前提照合**（`SimCache::discard_if_stale`）。
  `Evaluator::sim_premise_hash` が上流サブグラフ（配線と `NodeOutput`
  バインディングで届くノード）の構造・種別・bypass・ポート・全パラメータ
  （キーフレーム・式を含む）と fps・コンプ基準をハッシュする。位置・ラベル・
  色・z と上流でないノードは入れない。`Premise` が直近のグラフを
  `Graph::ptr_eq` で覚え、同じグラフなら再計算しない。
- 解像度・品質はハッシュに入れない（プレビュー ↔ 最終の切替で全破棄に
  なるため）。`step` はこれらを読まない契約（`StatefulProcessor` の doc）。
- scope 統合: トラックは `invalidate_scope` では**落とさない**（同じレイヤー
  ネットワーク内の無関係な編集で落ちるため）。落とす経路は
  `sim_prune_layer`（`set_document` が、ネットワークが変わったレイヤーと
  削除されたレイヤーに対して呼ぶ。ノードがもう無いトラックだけを解放）と
  `drop_sim_under`（shift / iteration スコープの退役。`PathId` の再発行前に
  必須）。シェル側の編集（`layer_shell_changed`）はトラックに触れない。
- トラックの鍵は `TimeShift` を除いたスコープ（`sim_scope`）。状態は
  ノード・フレーム・上流の前提の関数で、シフトは見るフレームを変えるだけなので
  シフトされた視点は 1 本のトラックを共有する。`Layer` / `Subnet` / `Comp` /
  `Iteration` は上流そのものを変えるので別トラックのまま。

### 単位 3（SIM-3）

- 評価器側（`eval/sim.rs`）: `Evaluator::set_sim_provisional(Some(n))` を
  立てると、`sim_state_or_provisional` が n フレームを超える前方ジャンプを
  「トラックの最後の状態」で答え（`ProvisionalSim` を記録）、トラックが空なら
  先に開始フレームだけ充填する。暫定で答えたノードと下流はノードキャッシュに
  古いフレームの値を入れてしまうので、`settle_provisional` が
  `mark_dirty_at` で落とす（確定の pull が実状態で再計算するため）。
  `set_sim_cancel` は充填のフレーム間で問い合わせる中断判定で、中断時は
  `EvalError::Cancelled`、計算済みのフレームはトラックに残る。
- サービス側（`runtime/eval_service.rs`）: `EvalServiceConfig::sim_jump_frames`
  （既定 `None` = 無効）。ワーカーは対象の評価を `run_targets` に括り出し、
  暫定が出たら `EvalUpdate { provisional: true }` を先に publish →
  `settle_provisional` → 新しい要求が来たら中断する判定を付けて同じ要求を
  もう一度 `run_targets` で評価（充填の続き）→ 確定値を同じ世代で publish。
  充填中に新しい要求が来たら確定値は publish せず、次の要求へ進む。
  出力段フレームキャッシュへの insert は確定した pass の後（`Pass::finished`）
  にだけ行うので、暫定の絵は入らない。投機（先読み）は暫定を使わない。
- 「評価スレッドプール」: 充填は `ravel-eval-service`（評価ワーカー、UI と別
  スレッド）上で走る。`EvalPool`（rayon）はサービスから使われていないので、
  新たにプールへ載せ替えてはいない。
- 消費側（`ravel-app` の `ProjectState`）: fence は（世代, 暫定か）の組。同じ
  世代の確定値は暫定値を置き換え、逆はしない。世代を代入する fence
  （`fence_at`）は暫定状態を消すので、fence された要求の確定値は通らない。
  ビューアのサービスは `viewer_eval_config` で `sim_jump_frames = Some(24)`。
  暫定フレームは通常のフレームと同じ経路で表示し、専用の表示は足していない
  （既存のパターンが無い）。

### 単位 4（SIM-4）

- `procedural-geometry.md` の sim 節と既存コードへの影響表、`REQ-CORE.md` の
  REQ-CORE-011 / REQ-CORE-006 の受入条件、`architecture.md`（キャッシュ節と
  前提照合表）、`agent-api-reference.md`、`docs/dev/add-node.md` を実装に合わせた。

### 追補（#639 内: チェックポイント・鍵・ハッシュ・アプリ配線）

- `SimTrack` は保持フレームだけの疎な列（`Vec<(frame, Held)>`、覆う範囲は
  `end` で持つ）と `stride` を持つ。1 件の課金は状態のバイト数 + 記帳分
  （`ENTRY_OVERHEAD`）で、記帳のコストも保護枠に入り、間引きで返る。
  `push` が前の最新フレームをチェックポイントでなければ捨て、`thin_to(cap)`
  が `stride` を倍にしながら間引く。`cap` はトラック 1 本あたり
  `CacheBudget` の sim 保護枠（`sim_reserved`）。予算の無い評価器は間引かない。
  `SimCache::lookup` が `Hit` / `Thinned { from, state }` / `Beyond` を返し、
  `Thinned` は `replay_sim_frames` が保存せずに `step` する。
- 前提ハッシュは `bincode` による `parameters` の構造的な直列化（float は
  生のビット）で、`Debug` 文字列ではない。範囲は変えていない。
- 性能: `perf-baseline.md` の「ステートレス評価パスの退行確認」。

### 単位 5（SIM-5）

- **束縛の表現**: `Bindings` は値の列（`values`）と、任意の `Rebind`
  （別のフレームで値を作り直す手段）を持つ型になった。`evaluate_sub` を呼ぶ
  プロセッサが `Bindings::rebindable(values, Arc<dyn Rebind>)` を渡す。
  作る 4 つのうち `subnet`（`SubnetPins`）・`geometry.iterate`（`PieceAt`）・
  `comp.network`（`LowerStack`、調整レイヤーの `source`）は `Rebind` を渡し、
  `layer.ref` は何も束縛しないので渡さない。`Rebind::rebind` は
  `EvalScope::process_args_at`（処理中のノードの入力とパラメータを任意の
  フレームで解決する。`sim_inputs_at` の一般化）で入力を引き直し、最初に値を
  作った関数をそのまま呼ぶ（規則は 1 つ）。
- **作ったスコープへの戻り方**: `evaluate_sub` が最初に入るとき `Anchor`
  （path / active_scopes / bindings / processing の各スタックの深さと path_id）
  を `Bindings` に記録し、シフトされた写しも持ち回る。`bindings_at(at)` は
  `at_anchor` で各スタックを anchor まで切り戻して（`split_off`、戻りで必ず
  復元）`rebind` を走らせる。作ったノード・グラフ・パス・束縛がそのまま
  見えるので、`evaluate_input_at` が辺を引くグラフも `scope_owners` のオーナーも
  正しく、引き直しのキャッシュは `<作ったスコープ>/TimeShift(作ったノード, f)`
  に入る（現在フレームのエントリを汚さない。作ったノードの dirty が
  `drop_time_shift_scopes` で道連れに落とす）。再入の循環検出は切り戻した
  `active_scopes` で行う。`evaluate_time_shifted` は `bindings_at(shifted)` を
  読む。同じフレーム・時刻なら引き直さず、シフトなしの評価は
  `Arc` 1 個（`Rebind`）と `Anchor` の記録が増えるだけ。
- **前提**: `sim_premise` が上流の走査で `net.in` に届いたら、束縛を作った
  スコープのノード（見出しと上流。内側のネットワークは sim 自身のスコープなので
  入れない）を同じ規則でハッシュし、そこでも `net.in` に届く限り外へ辿る。
  走査した各ノードは `scan_reads` で文書への読みを集める: `layer.ref`（参照先
  レイヤーのネットワークとシェル、参照の閉包。対象が静的に決まらなければ
  コンポの全レイヤー）、`layer.info` / `comp.info`（コンポ単位のシェル）、
  殻ノード（ID を復号。`comp.network` ならレイヤーのネットワークも）。集めた
  読みは文書から 1 回ハッシュする（`ExternalReads::hash_from`）。何も読まない
  sim のハッシュは従来のまま。
- **引く時点の照合**: 外を読む `Premise` は `ExternalStamp`（文書の `Arc` と、
  束縛を作ったスコープごとのグラフ）を持ち、高速経路（`SimCache::known`）は
  グラフに加えてそれらが現在と同一オブジェクトであることも確かめる。
  外を読まないトラックの経路は変わらない。
- **無効化エポック**: 処理系の差し替えや素材の再読み込みは構造ハッシュに映らない
  ので、`Evaluator` がノードごとの無効化エポック（`invalidate_node` と、それを
  呼ぶ `register`）を持ち、`sim_premise` が上流で走査したノード（サブネット内の
  ノードと、束縛を作ったスコープ側の走査を含む）のエポックをハッシュする。
  エポックを持つノードだけが寄与するので、無関係なノードの追加で値は動かない。
  `Premise` は作成時の時計を持ち、高速経路は時計が進んでいれば再計算する（落とす
  のは再計算の結果が変わったときだけ）。これで「`invalidate_node` が落とすのは
  そのノード自身のトラックだけ」という従来の制約は無くなった（`invalidate_node` は
  引き続き自分のトラックも即座に落とす）。
- **調整レイヤーの写像**: `LowerStack` は、ネットワーク内の時間でのフレーム差
  `d` を、コンポ時間の同じ `d` フレームとして `source` に適用する。タイムリマップ
  には逆写像が無いので、リマップ後のフレームからコンポ時間へ戻す方式は取れず、
  リマップの有無で規則を変えない（リマップ下でもこの規則を固定するテストがある）。
  `geometry.iterate` の `PieceAt` は、シフト先のフレームでピースが足りなければ空の
  ジオメトリを渡す。シェル読みの比較はコンポ単位（別レイヤーのシェル編集でも外を
  読む sim は落ちる。`invalidate_shell_readers` と同じ粒度）。
