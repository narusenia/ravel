# medium — ravel-core（評価器・ジオメトリ・undo）

深刻度 medium の課題を領域単位でまとめる。各項目は独立して着手可能。

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

## MED-CORE-02 | perf | 調整レイヤーのスコープキャッシュが毎フレーム全破棄される

**該当**: `crates/ravel-core/src/eval.rs:1321-1337`（バインディング構築側は `crates/ravel-nodes/src/comp/mod.rs:139-153`）

`evaluate_sub` はバインディングを `Arc::ptr_eq` で比較する（`eval.rs:206-210`）。
調整レイヤーの `source` バインディングは合成された下位スタックなので、
下に時間依存要素があれば毎フレーム新しい `Arc` が来る。
結果 `bindings_changed` が常に true になり
`self.cache.retain(|k, _| !k.path.starts_with(&path))` が
**そのスコープ内の全キャッシュ値**を破棄する — `net.in` の `source` ポートに依存していない
静的ジェネレータ・定数・ジオメトリまで含めて。

再生中、全調整レイヤー内の全ノードが時間依存性に関係なく毎フレーム再計算される。

**修正方針**: インターフェースノードのバインド済みポートから実際に下流に到達するノード集合を
スコープごとに1回計算し、そのキーだけを無効化する。
またはインターフェースノードのポート単位キャッシュをバインディング識別子でキーにする。

---

## MED-CORE-03 | bug | キャッシュ有効判定が `ctx.time` を無視 — 同一フレームのサブフレーム pull が stale 値を返す

**該当**: `crates/ravel-core/src/eval.rs:1042-1058`（エントリ格納は `:413-425`）

`CacheEntry` は `EvalContext` 全体を保持するが、有効判定は解像度・fps・bypass フラグと、
時間依存ノードについては**整数 `frame`** のみを比較する。
同じ `frame` で `time` が異なる連続 pull（サブフレーム位置。エンジンは
`EvalContext::sample_frame`、`layer_network_context` のサブフレームオフセット、
`world_matrix` のサブフレームテストで明示的にサポート）では、
時間依存ノードすべてが1回目の結果を返す。

現状これを踏む呼び出し元は無い（latent）が、サブフレーム機構はまさにモーションブラー・
タイムリマップのために作られている。発現時は「モーションブラーの N サンプルが全部同一」
という無エラーの症状になる。

**修正方針**: 時間依存ノードのフレーム進行チェックに `entry.ctx.time != ctx.time`
（または導出した `sample_frame()`）を含める。同一フレーム・異サブフレーム時刻での
2回 pull の回帰テストを追加。

---

## MED-CORE-04 | bug | 評価とサブネット再帰走査に深さ上限が無い — 深いグラフでスタックオーバーフロー

**該当**: `crates/ravel-core/src/eval.rs:840-1178`

`eval_node` は `pull_input` を通じて再帰する（連鎖ノード1つあたり2スタックフレーム、
各フレームが複数の `Vec` / キーを保持）。
モジュールドキュメントは循環安全性を保証するが、**深さ**は一切制限していない。
数千ノードの直線チェーン（プロシージャルグラフでは現実的。テストは 100 まで、`eval.rs:2098`）で
ワーカースレッドのスタックを溢れさせプロセスが abort する（バックグラウンドスレッドの
オーバーフローは catch 不能）。

同じパターンが全サブネット再帰走査にある — `check_unique_node_ids`
(`composition/mod.rs:567-580`)、ロード時の `normalize_*`、`Graph` のデシリアライズ
(`graph.rs:293-297`)。深くネストしたサブネットを持つ細工済み / 破損した `.ravprj` や
ジャーナルは、`Document::validate` を迂回してロード時にアプリをクラッシュさせられる。

**修正方針**: `eval_node` を明示的なワークスタックに変換する（または評価ワーカーを
大きい固定スタックで spawn し、文書化された深さ上限を超えたら `EvalError` を返す）。
サブネットのデシリアライズ・検証にネスト深さ上限を追加。

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

## MED-CORE-06 | perf | 評価結果キャッシュにメモリ上限が無い — ノードごとにフレームバッファ1枚を永久保持

**該当**: `crates/ravel-core/src/eval.rs:1113-1121`（値型は `types.rs:168-187`）

処理済みノードの出力は `NodeKey` ごとにキャッシュされ、無効化以外では退避されない。
1080p RGBA f32 の CPU `FrameBuffer` は約 33MB。
コンパイル済みシェルチェーンだけでレイヤーあたり3〜4枚のフレームバッファノードを生む
（network / transform / opacity / merge）ため、10レイヤーのコンポジションで
約 1GB の前フレームバッファ（または VRAM の GPU 常駐相当分）を、
ユーザーがそこから離れた後も保持し続ける。
ジオメトリ出力と全レイヤーネットワーク中間結果も加算される。サイズ追跡も LRU も無い。

**修正方針**: サイズ考慮の退避ポリシーを追加。エントリごとの概算バイト数を追跡
（`NodeData::approx_size()` や `is_gpu_resident` を考慮した重み）し、
設定可能な予算を超えた分を LRU 退避する。
代替として、中間（出力ピン留めされていない）フレームバッファを下流消費後に即破棄。

---

## MED-CORE-07 | debt | `scope_owners` / `scope_bindings` が pruning されない、`register` が毎回キャッシュ全走査

**該当**: `crates/ravel-core/src/eval.rs:492-498`（他 `:519-546`, `:747-767`）

`invalidate_scope` は削除レイヤー / サブネットのキャッシュ・dirty エントリを消すが、
`scope_owners` と `scope_bindings` のエントリ（`Bindings` = `Arc<dyn NodeData>`、
フレームバッファを含みうる）を残す。
長いセッションで多数のレイヤーを削除すると保持フレームがリークする。

別途 `register()` はノードのパスを探すため `cache` と `dirty` の全体をイテレートする
(`:521-532`)。`Params` 無効化ヒントではホストがパラメータ変更ティックごとに
プロセッサを再登録するため、スクラブ中は変更ノードごと・ティックごとに
O(キャッシュサイズ) の走査が走る。

**修正方針**: `invalidate_scope` / `set_document` 内で `scope_owners` / `scope_bindings` を
プレフィックスで prune。NodeId→paths の逆引きインデックスを維持する
（または MED-CORE-01 のインターン方式を使う）ことで `register` の走査を廃止。

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
[CRIT-03](../critical/CRIT-03-project-write-not-atomic.md)（唯一の防御線が非アトミック保存）
