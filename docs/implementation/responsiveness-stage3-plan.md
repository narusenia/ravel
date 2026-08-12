# 応答性 第3段 実装計画（フェーズ C3）

> **Status**: Planned — `HIGH-01` / `HIGH-02` / `MED-CORE-01` / `MED-CORE-05` /
> `MED-UI-01`〜`MED-UI-06` / `MED-GPU-04` / `MED-GPU-05`
>
> この文書は設計ゲート用の実装計画である。この計画そのものでは `crates/`
> 配下のコードを書かない。実装時は単位ごとに分割し、各単位の完了条件を
> 満たしてから次へ進む。

## 問題

第1段（`done/ui-responsiveness-plan.md`、`CRIT-01` / `HIGH-06` / `HIGH-07`）と
第2段（`gpu-compositing-plan.md`、`HIGH-04` / `HIGH-05` / `HIGH-08`）は
**呼ばれる回数**と**1 ピクセルあたりのコスト**を落とした。第3段が扱うのは
残った軸 — **要素数に対する 1 回あたりのコスト**である。

現状、次の量がすべてプロジェクト規模に線形または二次で効く。

| 経路 | スケールする量 |
|---|---|
| `eval_node` の入力エッジ収集 | ノード訪問 × 全エッジ |
| `mark_dirty_at` の下流探索 | dirty ノード × 全エッジ |
| `changed_network_paths` | 変更のあった comp の全レイヤーの deep compare |
| `NodeKey` の構築 | ノード訪問あたり 3〜4 回のパス `Vec` 確保 |
| `attribute_transfer` | source 点数 × target 点数 |
| `document_changed` → `compiled_root` | 編集ティックあたりレイヤー数 |
| Properties `refresh_values` | 再生フレームあたり 2 回 × 全セクション |
| Timeline `build_layer_headers` / キャンバス | 全レイヤー（垂直カリング無し） |
| Timeline `sync_from_project` | notify あたり Composition の deep compare |
| Outliner / MediaBin `rebuild_rows` | notify あたり全 comp・全レイヤー・全ノード |
| `raster_paths` | プリミティブ数 × 全画面 |
| `ensure_gpu` | 同一フレームを消費 GPU ノード数だけ再アップロード |

`HIGH-17`（sws スケーラの毎フレーム再生成）は第3段の対象だったが
[closed](../../issues/closed/HIGH-17-sws-scaler-recreated-per-frame.md) 済みで、
メディアデコードのクラスタはこのフェーズに残っていない。

### 既にある材料

ゼロから作る必要があるものは少ない。第2段までで次が入っている。

- `Graph::ptr_eq`（`crates/ravel-core/src/graph.rs:832`） — 永続マップの根が
  同一なら内容も同一。O(1)。**インデックスのキャッシュキーに使える**
- `Graph::downstream_adjacency`（同 `:863`） — ワイヤと `NodeOutput`
  パラメータ束縛の両方をまたぐ下流隣接。`ScopeReach::of` が既に使っている。
  ただし**呼び出しごとに構築**しており、キャッシュされていない
- `Document::changed_network_paths` の comp 単位短絡
  （`composition/mod.rs:1230` の `Arc::ptr_eq`） — レイヤー単位には未適用
- `ProjectState.revision`（`project_state.rs:271`） — ドキュメント変更ごとに
  進むカウンタ。パネルの deep compare を置き換えられる
- `panels::MirrorEpoch`（`panels/mod.rs:960`） — `HIGH-07` が入れた
  epoch ゲート。5 パネルが既に持っている

### 制約付きの 2 単位

このフェーズには、**そのままでは着手できない**単位が 2 つある。どちらも
先行単位を C3 の中に置いて解く。

**`MED-UI-06`（2 経路の重複 sync）は測定できない。** issue が明記している
とおり、GPUI は 1 エフェクトサイクル内の `cx.notify()` を合流させるので、
observer 数を数えるプローブでは削減も回帰も観測できない。**sync 関数の
呼び出し回数を数える計装を先に入れる**（`RESP3-5`）。

**`MED-GPU-04`（CPU ラスタライズ）はゴールデンテストに固定されている。**
`processor_for_node` は `rasterize` の synthetic ノードを意図的に CPU 参照経路へ
落としており（`crates/ravel-nodes/src/lib.rs:150-154`）、`shape_layer_golden` が
その画素を pin している。`finalize` を GPU ラスタライザに切り替えると
このテストが落ちる。**ゴールデンを GPU / CPU 一致テストへ置き換える単位を
先に置く**（`RESP3-12`）。

## 目標アーキテクチャ

### 隣接はグラフのバージョンごとに 1 回だけ作る

`Graph` は immutable で、`ptr_eq` が「同じ内容」を O(1) で証明する。だから
隣接インデックスは**グラフ本体ではなく評価器側にキャッシュ**し、`ptr_eq` が
外れたときだけ作り直す。グラフに可変フィールドを足すと `im` の構造共有
（clone が安いこと）と `Graph` の値としての単純さが壊れる。

```text
Evaluator
  └─ AdjacencyCache
       ├─ graph: Graph            (ptr_eq の照合用に保持)
       └─ index: Adjacency
            ├─ inbound:  NodeId → [EdgeId]   (eval_node)
            └─ outbound: NodeId → [NodeId]   (mark_dirty_at)
```

`Graph::inputs_of` / `outputs_of` は公開 API として残す（`network.rs` が
使っている）が、最内ループはインデックス経由にする。

### パスはインターンして `NodeKey` を `Copy` にする

`NodeKey { path: Vec<PathSegment>, node: NodeId }` はノード訪問あたり
3〜4 回 clone される。スコープ進入時に `Vec<PathSegment> → PathId(u32)` を
1 回だけ割り当て、`cache` / `dirty` / `run` / `visiting` のキーを
`(PathId, NodeId)` の `Copy` 型にする。ハッシュは O(1)、ノードあたりの
確保はゼロになる。

インターナは `Evaluator` が所有する。パスは評価の間しか意味を持たず、
外へ漏らすと `PathId` の有効範囲が追えなくなる。**公開 API の
`NodeKey` は `Vec<PathSegment>` のまま**にし、内部キーとの変換を境界に置く。

### パネルは revision で門を作り、Composition を比較しない

Timeline / Outliner / MediaBin が持っているのは**ドキュメントの鏡**である。
鏡が古いかどうかは `ProjectState.revision` が答えられるので、`Composition`
の deep compare は要らない。`MirrorEpoch` は既にその形をしているが、
`sync_from_project` 側が使っていない。

グローバル駆動の sync（`ActiveComposition` / `SelectedPropertiesTarget`
observer）は生きたドキュメントから読み直すので、その epoch は既にカバー
されている。**sync のあとに現在の epoch を記録する**だけで、対になる
project notify を吸収できる。

### GPU 常駐フレームは値側に持つ

`ensure_gpu` はプールテクスチャを取ってアップロードし、ディスパッチ直後に
解放する。同じ `Arc<FrameBuffer>` が N 個の GPU ノードに供給されると N 回
アップロードされる。**変換結果を評価器キャッシュに載る値として持つ**
（最初の GPU 消費側が `GpuFrameBuffer` へ変換して返す）のが本筋だが、
評価器の値型を触るのは影響範囲が広い。**フレーム内のソースバッファ
ポインタでメモ化する**方を採る（後述「やらないこと」）。

## 実装単位

3 クラスタ、14 単位。クラスタ内は依存順、クラスタ間は独立。

### クラスタ A: 評価器とグラフ（`ravel-core`）

| 単位 | 内容 | 依存 | 引受 issue |
|---|---|---|---|
| `RESP3-1` | 隣接インデックスと `ptr_eq` キャッシュ | — | `HIGH-01` |
| `RESP3-2` | レイヤー単位の `ptr_eq` 短絡と親インデックス | — | `HIGH-02` |
| `RESP3-3` | パスのインターンと `Copy` な内部キー | `RESP3-1` | `MED-CORE-01` |
| `RESP3-4` | `attribute_transfer` の空間分割と近傍打ち切り | — | `MED-CORE-05` |

### クラスタ B: パネル 1 回あたりのコスト（`ravel-app` / `ravel-ui`）

| 単位 | 内容 | 依存 | 引受 issue |
|---|---|---|---|
| `RESP3-5` | sync 呼び出し回数の計装 | — | （`MED-UI-06` のゲート） |
| `RESP3-6` | `Params` ヒントでコンパイル済みチェーンを保持 | — | `MED-UI-01` |
| `RESP3-7` | Properties の refresh 重複排除と非表示スキップ | `RESP3-5` | `MED-UI-02` |
| `RESP3-8` | Timeline の垂直カリング | — | `MED-UI-03` |
| `RESP3-9` | Timeline の revision ゲート | `RESP3-5` | `MED-UI-04` |
| `RESP3-10` | Outliner / MediaBin の revision ゲート | `RESP3-5` | `MED-UI-05` |
| `RESP3-11` | グローバル駆動 sync の epoch 記録 | `RESP3-9`, `RESP3-10` | `MED-UI-06` |

### クラスタ C: GPU ディスパッチ（`ravel-nodes`）

| 単位 | 内容 | 依存 | 引受 issue |
|---|---|---|---|
| `RESP3-12` | rasterize ゴールデンの GPU / CPU 一致テスト化 | — | （`MED-GPU-04` のゲート） |
| `RESP3-13` | `finalize` の GPU ラスタライズと CPU 経路の bbox 限定 | `RESP3-12` | `MED-GPU-04` |
| `RESP3-14` | `ensure_gpu` のフレーム内メモ化 | — | `MED-GPU-05` |

## 単位ごとの完了条件

### `RESP3-1` 隣接インデックスと `ptr_eq` キャッシュ

- `Evaluator` が `Graph::ptr_eq` をキーにした隣接インデックスを保持する。
  グラフが差し替わったときだけ再構築する
- `eval_node` の入力エッジ収集（`eval.rs:2270-2271` の
  `edges().filter(|e| e.target == node)`）がインデックス経由になる
- `mark_dirty_at`（`eval.rs:2023`）の `graph.outputs_of(current)` が
  インデックス経由になり、呼び出しごとの `Vec` 確保が消える
- `Graph::inputs_of` / `outputs_of` は公開 API として残す
  （`network.rs:1605`, `:1616` が使っている）
- サブネット再帰でスコープごとに別グラフを見るとき、インデックスが
  取り違わないこと（グラフ単位でキャッシュを引く）をテストが落とす
- 1,000 ノード / 1,500 エッジ規模の評価ベンチマークを
  `crates/ravel-nodes/examples/perf_baseline.rs` に足し、before / after を
  計画書またはこの節に記録する
- 既存のゴールデン・評価テストが無改変で通る

### `RESP3-2` レイヤー単位の `ptr_eq` 短絡と親インデックス

- `Document::changed_network_paths`（`composition/mod.rs:1226`）の
  `old_layer.network != layer.network` が `Graph::ptr_eq` の短絡を先に通る
- `Graph::eq` 自体もノード `Arc` の `Arc::ptr_eq` を先に見る
  （`im::HashMap::ptr_eq` の全体短絡を含めるかは実装判断）
- `set_document` の祖先チェーン再構築が、レイヤーごとの O(L) `get_layer`
  から親インデックス経由の O(1) ルックアップになる
- **無変更のレイヤーが `changed_network_paths` に載らないこと**をテストが
  落とす（構造共有を保った編集を作り、変更したレイヤーだけが返ることを検査）
- 既存の `changed_network_paths_detects_edits`（`:2230`）が無改変で通る

### `RESP3-3` パスのインターンと `Copy` な内部キー

- `Evaluator` が `Vec<PathSegment> → PathId(u32)` のインターナを持つ
- `cache` / `dirty` / `run` / `visiting` の内部キーが `(PathId, NodeId)` の
  `Copy` 型になり、ノード訪問あたりのヒープ確保がゼロになる
- 公開 API の `NodeKey` は `Vec<PathSegment>` のまま。境界で変換する
- `evaluate_sub` のスコープ進入で `scope_owners` / `scope_bindings` 用の
  パス clone が消える
- スコープを跨いだキャッシュ衝突が起きないこと（同じ `NodeId` が別スコープで
  別の値を持つ）をテストが落とす
- `perf_baseline` の評価シナリオで確保回数が減ることを記録する

### `RESP3-4` `attribute_transfer` の空間分割と近傍打ち切り

- `Nearest` が呼び出しごとに 1 回だけ一様グリッド（または kd-tree）を構築し、
  ターゲット点ごとの全ソース線形走査（`geometry/ops.rs:510-538` の
  `nearest_index`）が消える
- `DistanceWeighted` が k 近傍または半径で打ち切り、
  長さ `source_count` の `Vec<f32>` をターゲット点ごとに確保しなくなる
- 打ち切りの既定値と、それがパラメータで変えられるかを決めて文書に書く
- **打ち切りによる結果の差**が受け入れ範囲であることを、既存のゴールデンか
  新規の数値テストで示す。既存ゴールデンが変わるなら、変わる理由を
  PR 本文に書く
- 10k → 10k の転送でベンチマークの before / after を記録する

### `RESP3-5` sync 呼び出し回数の計装

- パネルの sync 関数（Properties `refresh_values` / Timeline
  `sync_from_project` / Outliner・MediaBin `rebuild_rows`）の**実行回数**を
  数える計装が入る。`tracing` span カウンタか、テストから読める
  カウンタのどちらでもよい
- 計装は**リリースビルドで測定コストを持たない**か、持つなら
  `debug_assertions` か feature で切れる
- ドラッグ 1 回・再生 1 秒あたりの実行回数を測る手順が
  `docs/implementation/perf-baseline.md` に載る
- 計装を使った現状値（`RESP3-7`〜`RESP3-11` の before）を記録する

### `RESP3-6` `Params` ヒントでコンパイル済みチェーンを保持

- `document_changed`（`project_state.rs:763` 付近）が
  `InvalidationHint::Params` でコンパイル済みチェーンを破棄しなくなる
- `Structural` ヒントとコンポジション切替のときだけ再構築する
- **パラメータ編集がビューアに反映されること**を落とすテストがある
  （チェーンを保持したまま値だけ変わる経路）
- レイヤーの追加・削除・並べ替え・parent 変更が `Structural` として
  届いていることを確認し、届いていないヒントがあれば直す

### `RESP3-7` Properties の refresh 重複排除と非表示スキップ

- `PlaybackPosition` observer と project observer の両方から来る
  `refresh_values` が、**1 フレームあたり最大 1 回**に合流する
- パネルが非表示のとき `refresh_values` を走らせない
- アニメーションチャンネル由来のフィールドだけを再構築する
  （静的なパラメータ行の文字列を毎フレーム作り直さない）
- `RESP3-5` の計装で、再生 1 秒あたりの実行回数が半分以下になることを記録する
- スクラブ中に値が止まって見える回帰が無いことを、テストか実機確認で示す

### `RESP3-8` Timeline の垂直カリング

- `build_layer_headers`（`panels/timeline.rs`）が可視 y 範囲の行だけを構築する
- レイヤー領域のキャンバス描画ループが、レーン境界・バー・キーフレームを
  可視 y 範囲に限って描く
- 行レイアウトの算術（`row_at_content_y`）を再利用し、行高の計算を二重に
  持たない
- 100 レイヤーのコンポジションで、構築される行数が可視行数に比例することを
  テストが落とす
- スクロール端で行が欠ける／二重に出る回帰が無いこと

### `RESP3-9` Timeline の revision ゲート

- `sync_from_project`（`panels/timeline.rs:354` 付近）が
  `self.state.composition() != comp.as_ref()` の deep compare をやめ、
  `ProjectState.revision` と `MirrorEpoch` で門を作る
- 鏡または選択が実際に変化したときだけ `cx.notify()` する
- レンダーごとの `self.state` clone 箇所（`:2385`, `:2557`, `:2773`,
  `:2793`, `:3432`）のうち、参照で足りるものを参照にする
- **revision が進んでいないのに sync が走らないこと**、**進んだら必ず走ること**
  の両方をテストが落とす
- `RESP3-5` の計装で、ドラッグ 1 回あたりの sync 回数の before / after を記録する

### `RESP3-10` Outliner / MediaBin の revision ゲート

- `rebuild_rows`（`panels/outliner.rs:146` 付近、`panels/media_bin.rs:78`
  付近、`ravel-ui/src/panels/outliner.rs:181` 付近）がドキュメント
  revision チェックでゲートされる
- 評価更新の経路からこれらのパネルへ notify が届かないことを確認する
  （`CRIT-01` の修正で大半は解消しているはずなので、**残っているかを
  まず測る**。残っていなければその旨を PR 本文に書き、ゲートだけ入れる）
- 行ラベル文字列の確保が revision 不変のとき起きないことをテストが落とす

### `RESP3-11` グローバル駆動 sync の epoch 記録

- `ActiveComposition` observer と `SelectedPropertiesTarget` observer が
  sync したあと、`MirrorEpoch` に現在の epoch を記録する
- 対になる project notify が同じ epoch を見て sync を飛ばす
- **ノードパラメータのドラッグで Properties の再解決が move あたり 1 回に
  なること**を、`RESP3-5` の計装で示す
- **コンポジション切替で Timeline / Outliner の sync が 1 回になること**を
  同じ計装で示す
- グローバル書き込みだけが起きて project notify が来ない経路（あるなら）で
  鏡が古いまま残らないことをテストが落とす

### `RESP3-12` rasterize ゴールデンの GPU / CPU 一致テスト化

- `shape_layer_golden`（`crates/ravel-nodes/tests/shape_layer_golden.rs`）が
  「CPU 経路の確立済み画素を pin する」形から、**CPU と GPU が許容誤差内で
  一致することを検査する**形になる
- 許容誤差の根拠（アルファ規約、フィルタ、丸め）をテスト内のコメントに書く
- GPU が無い環境（CI の一部）でテストがどう振る舞うかを決める
  — skip するなら skip したことが出力に残ること
- **`processor_for_node` の synthetic 分岐（`lib.rs:150-154`）はこの単位では
  変えない**。テストの形だけを先に変え、切り替えは `RESP3-13` で行う
- 既存の CPU 経路が無改変で通ることを確認する

### `RESP3-13` `finalize` の GPU ラスタライズと CPU 経路の bbox 限定

- `GpuEvalHooks::finalize`（`crates/ravel-nodes/src/eval_hooks.rs:250` 付近、
  `:313` の `RasterizeProcessor::from_node`）が、スコープ内の `GpuContext` と
  プールを使って GPU ラスタライザを構築する
- `processor_for_node` の synthetic 分岐が外れる（または GPU 版を返す）
- CPU 経路が残る箇所では、カバレッジバッファを**プリミティブごとに確保せず
  1 枚を再利用**し、`blend_coverage` をプリミティブの bbox に限定する
  （`crates/ravel-nodes/src/rasterize/mod.rs:677`, `:686-694`）
- `RESP3-12` の一致テストが通る
- シェイプ / スキャッターをプレビューしながらのスクラブで、CPU
  ラスタライズが毎フレーム走らないことを計測で示す

### `RESP3-14` `ensure_gpu` のフレーム内メモ化

- `ensure_gpu`（`crates/ravel-nodes/src/gpu_util.rs:59-78`）が、同一評価内で
  同じソース `FrameBuffer` に対して 1 回だけアップロードする
- メモの寿命が**評価 1 回**に閉じる（フレームを跨いで古いテクスチャを
  返さない）ことをテストが落とす
- プールテクスチャの lease がメモの寿命と整合する（メモが持っている間は
  解放しない）
- N 個の GPU 消費ノードを持つグラフで、アップロード回数が N から 1 に
  なることをテストが落とす
- 4K RGBA32F のシナリオで帯域の before / after を記録する

## やらないこと / 見送る選択肢

- **`Graph` 本体に可変の隣接フィールドを持たせない。** `im` の構造共有で
  clone が安いことと、`Graph` が値であることが壊れる。キャッシュは
  `Evaluator` 側に置く（`RESP3-1`）
- **`PathId` を公開 API へ出さない。** インターナの寿命が評価器に閉じている
  ことが `PathId` の正しさの根拠なので、外へ出した時点で保証が消える
  （`RESP3-3`）
- **`GpuFrameBuffer` を評価器の値型に足さない。** 影響範囲が
  `ravel-core` の値型・キャッシュ・シリアライズに及び、C3 の目的
  （1 回あたりのコスト）に対して大きすぎる。フレーム内メモ化で足りるかを
  先に測る（`RESP3-14`）。足りないと分かったら別計画で扱う
- **UI 側に新しい仮想リストの基盤を作らない。** Timeline の垂直カリングは
  既にある行レイアウトの算術で足りる。`uniform_list` への移行は
  カリングだけでは足りないと測れてから（`RESP3-8`）
- **`attribute_transfer` に汎用の空間分割基盤を作らない。** 一様グリッドを
  この 1 ノードの中に閉じて書く。他の geometry ops が要ると分かってから
  `geometry/` へ引き上げる（`RESP3-4`）
- **非同期リードバックは扱わない。** `GPUCOMP-10` で不要と判断済み

## ロードマップ上の位置づけ

`roadmap.md` のフェーズ C3「応答性の残り」。キャッシュ（C2）の後に置く根拠は
`MED-UI-01` と `MED-CORE-01` がキャッシュの無効化経路と同じコードを触るため。
`CACHE-2`〜`4` が `CacheIdentity` と無効化の粒度を書き換えたので、その後に
測り直さないとどれが本当に残っているか分からない。

`RESP3-1` / `RESP3-2`（`HIGH-01` / `HIGH-02`）は依存が無く、体感が痛いときは
先行して着手してよい。

## 関連文書

- `roadmap.md` フェーズ C3
- `done/ui-responsiveness-plan.md`（第1段）
- `gpu-compositing-plan.md`（第2段）
- `cache-plan.md`（`CacheIdentity` と無効化の粒度）
- `perf-baseline.md`（測定シナリオ）
- `issues/README.md`（個票）
