# medium — ravel-core（評価器・ジオメトリ・undo）

深刻度 medium の課題を領域単位でまとめる。各項目は独立して着手可能。

> **例外**: `MED-CORE-02` / `03` / `06` / `07` は
> `docs/implementation/cache-plan.md` が引き受ける（それぞれ CACHE-4 / CACHE-2 /
> CACHE-3 / CACHE-3）。4 件すべてがキャッシュの同一性・予算・無効化という
> 同じ関数群を書き換えるので、**個別に直すと衝突する**。

---

## MED-CORE-01 | perf | `NodeKey` のパス `Vec` を訪問ごとに複数回 clone する

**該当**: `crates/ravel-core/src/eval.rs:848-851`（他 `:1127`, `:1319`, `:1336`）

`eval_node` ごとに `NodeKey { path: self.path.clone(), node }` を構築し、
さらに `visiting.insert` / `cache` 挿入 / `run.insert` 用に再 clone する
→ ノード訪問1回あたり 3〜4 回のヒープ確保 + フルパスのハッシュ計算。
`evaluate_sub` は加えてネストスコープ進入ごと（= レイヤーごと・フレームごと）に
`scope_owners` / `scope_bindings` 用のパス clone を行う。

浅いパスなら1回は小さいが、評価の最内ループに乗っている。

**修正方針**: パスをインターンする。スコープ進入時に `Vec<PathSegment>` → `PathId`(u32) を1回だけ
割り当て、`cache` / `dirty` / `run` / `visiting` を `(PathId, NodeId)` の `Copy` キーにする。
O(1) ハッシュ、ノードごとの確保ゼロ。

---

## MED-CORE-05 | perf | `attribute_transfer` が O(source×target)、ターゲットごとに重み `Vec` を確保

**該当**: `crates/ravel-core/src/geometry/ops.rs:120-133`（ヘルパー `:510-538`）

`Nearest` モードはターゲット点ごとに `nearest_index`（全ソース点の線形走査）を呼ぶ。
`DistanceWeighted` はターゲット点ごとに `normalized_weights` を呼び、
長さ `source_count` の `Vec<f32>` を確保して**全**ソース点との重みを計算する。
10k→10k の転送で 1億回の距離計算 + 1万回の Vec 確保 — 上流が動く限り毎フレーム。
ジオメトリ ops には空間分割構造が一切無い。

**修正方針**: `Nearest` は呼び出しごとにソース位置の一様グリッドまたは kd-tree を1回構築。
`DistanceWeighted` は近傍を打ち切る（k 近傍または半径。Houdini と同様）。
全域の逆距離重み付けは遅い上に視覚的には打ち切りカーネルと区別できない。

---

## MED-CORE-08 | debt | クラッシュ復旧ジャーナルとスレッディングランタイムが完全に未使用、かつ設計が実際の undo 単位を覆えない

**該当**: `crates/ravel-core/src/undo/journal.rs`, `undo/mutation.rs`, `undo/recovery.rs`,
`runtime/eval_pool.rs`, `runtime/decode_pool.rs`, `runtime/channels.rs`, `runtime/io_runtime.rs`

grep で確認: `ravel-core` の外から `JournalWriter` / `recover` / `GraphMutation` / `EvalPool` /
`DecodePool` / `eval_channel` / `decode_channel` / `reply_channel` / `io_runtime` を参照する
コードは無い。アプリが使うのは `UndoStack`（`ravel-ui/src/document.rs:90`、200件上限）と
単一スレッドの `EvalService` のみ。

「後で配線する」のを難しくしている構造的問題が2つ。

1. `GraphMutation` はフラットグラフ操作（Add/RemoveNode、エッジ、メタデータ）のみを covers するが、
   実際の undo / 永続化単位は `Document`（コンポジション、レイヤー、レイヤーネットワーク）
   → ジャーナルは現実の編集の大半を記録できない
2. `append` はミューテーションごとに `flush` + `sync_data`（`journal.rs:258-261`）
   → 編集経路に置くと対話操作ごとにミリ秒級の fsync が加わる

一方この未使用コードは実コストを払わせている。フォーマットバージョンは既に5回上がり（v2〜v6）、
bincode のフィールドレイアウト制約が `graph.rs` 全体の `InputPort` / `NodeMetadata` /
`ParameterValue` の設計コメントを縛っている。

**修正方針**: 二択を決める。(a) ジャーナルを `DocumentMutation` 粒度に昇格させ、
fsync をバッチ / 非同期にして実際に配線する。(b) 計画ができるまで journal / mutation / recovery と
未使用ランタイムプールを削除する。現状 bincode レイアウト制約はスキーマ変更ごとに税を課すだけで
何も買っていない。

**関連**: [medium/app-shell.md](app-shell.md) の MED-APP-11（アプリ側から見た同じ問題）、
[CRIT-03](../closed/CRIT-03-project-write-not-atomic.md)（唯一の防御線が非アトミック保存）

---

