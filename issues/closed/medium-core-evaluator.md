# closed / medium — ravel-core（評価器・ジオメトリ・undo）

解決済みの medium 項目。個票は起票時のまま残し、各項目の **解決済み** 行が結果を記録している。

未解決分は [`../medium/core-evaluator.md`](../medium/core-evaluator.md)。

---

## MED-CORE-01 | perf | `NodeKey` のパス `Vec` を訪問ごとに複数回 clone する

**該当**: `crates/ravel-core/src/eval.rs:848-851`（他 `:1127`, `:1319`, `:1336`）

> **解決済み**: `RESP3-3`（PR #395）。`PathId(u32)` のインターナが入り、
> `cache` / `dirty` / `run` / `visiting` の内部キーが `(PathId, NodeId)` の
> `Copy` になった。ノード訪問あたりのパス `Vec` 確保はゼロ。
> **ネストスコープの評価で約 35% 減**。ルートスコープでは差が出ない
> （そこで clone していたのは空の `Vec` なので元から安かった）。
> 公開 API の `NodeKey` は `Vec<PathSegment>` のまま、境界で変換する。

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

> **解決済み**: `CACHE-4` が無効化の粒度を 2 段で絞った（2026-07-31）。
> `binding_delta` が変わったバインディング名だけを出し、`ScopeReach`
> （スコープごと・グラフごとに 1 回、`Graph::ptr_eq` で再利用）がその名前の
> `net.in` 出力ポートから下流に到達するノード集合を求めて、そのキーだけを捨てる。
> 加えてインターフェースノードは `CacheMiss::BindingsChanged` で再計算し、
> **出力ポート単位で fresh を報告する**ので、`source` の差し替えが `t` や
> `base_geometry` の消費者を巻き込まない。到達先を追えないバインディング名
> （どのインターフェースポートにも一致しない、`net.in` が無い）は従来どおり
> スコープ全体を捨てる。回帰テストは
> `adjustment_scope_keeps_its_static_nodes_across_frames` /
> `changed_binding_spares_the_ports_it_does_not_back` /
> `changed_binding_recomputes_the_nodes_its_port_reaches` /
> `repeating_an_unchanged_scope_drives_the_hit_rate_to_one`。

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

> **解決済み**: `CACHE-2` が有効判定を `CacheIdentity` にまとめ、時間軸を
> 整数 `frame` から `TimeKey`（`EvalContext::sample_frame()` を 1/4096 フレームに
> 量子化）へ移した（2026-07-31）。同一フレーム・異サブフレーム位置の 2 回 pull は
> 別扱いになる。回帰テストは
> `sub_frame_positions_within_one_frame_are_evaluated_separately`。

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

## MED-CORE-05 | perf | `attribute_transfer` が O(source×target)、ターゲットごとに重み `Vec` を確保

**該当**: `crates/ravel-core/src/geometry/ops.rs:120-133`（ヘルパー `:510-538`）

> **解決済み**: `RESP3-4`（PR #395）。一様グリッドで `Nearest` が**厳密なまま**
> O(1) 近傍探索になり（10k→10k で 820 → 0.5 ms）、`DistanceWeighted` は
> 8 近傍で打ち切った（178 → 2.5 ms）。打ち切りは全域 IDW より**高精度**
> — 線形場に対する最大誤差が 0.46 対 9.63 で、遠い点の寄与は信号ではなく
> 平滑化だった。ターゲットごとの重み `Vec` 確保は 1 本の平坦バッファに置換。
> 近傍数（`DISTANCE_WEIGHTED_NEIGHBOURS`）はパラメータにしていない
> （ノードのシグネチャ変更になる。判断は計画書の「やらないこと」）。

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

> **解決済み**: `CACHE-3` が会計と退避を入れた（2026-07-31）。
> `NodeData::byte_size()`（既定実装なし）が概算バイト数を返し、`CacheStore` が
> エントリごとに `CacheBudget` の予約を持つ。予算超過で最終アクセスが最も古い
> ものから落ちる（ヒットで `touch` するので、毎フレーム読まれる値は残る）。
> 構造変更時の再同期は `Evaluator::reset()`（予算だけ残して状態を捨てる）を
> `EvalService` が呼ぶ形にし、フック側が `*evaluator = Evaluator::new()` で
> 予算ごと捨てられないよう `sync` の引数を `ProcessorSync` に絞ってある。
> GPU 常駐値は VRAM 層に計上され、`TexturePool` のアイドル枠はその残余になる
> ので、**VRAM の上限を決める場所が 1 つになった**。既定は VRAM 1 GiB /
> RAM 2 GiB（`CacheBudgetConfig`）。`settings.toml` の `[cache]` はパースと
> マージまでで、**起動時は既定値のまま**。走行中の予算へ流す配線は `SET-8`。
> 回帰テストは `the_budget_evicts_the_oldest_entry_and_holds_the_line` /
> `a_re_read_entry_outlives_an_untouched_one` /
> `evicting_a_value_releases_its_bytes_to_the_budget` /
> `a_shared_budget_pool_never_starves_across_the_vram_limit`。

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

> **解決済み**: `CACHE-3` が両方を潰した（2026-07-31）。
> `invalidate_scope` が `prune_scope_state` でプレフィックス一致の
> `scope_owners` / `scope_bindings` / `scope_reach`（`CACHE-4` が足した、
> `Graph` クローンを持つ）を捨て、`invalidate_all` は 3 つとも空にする。
> `register()` 側は `CacheStore` が `NodeId → paths` の逆引き索引を維持して
> `forget_node` を O(そのノードのパス数) にした。キャッシュ・dirty・索引・
> バイト会計は private モジュール `cache_store` に閉じ、`HashMap` を直接
> 触れる場所を無くしてある。回帰テストは
> `removing_a_layer_leaves_no_scope_state_behind` /
> `deleting_a_layer_through_the_document_prunes_its_scope_state` /
> `register_does_not_walk_the_cache` /
> `the_reverse_index_survives_every_kind_of_invalidation`。

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

## MED-CORE-09 | bug | `Composition.background_color` が保存も編集もできるのに評価されない

> **解決済み**: PR #213（2026-07-30）。殻コンパイルの最下段へ synthetic な
> `comp.background` を追加し、空コンプを含む評価結果へ RGBA 背景色を反映した。
> Viewer にはコンプ背景 / チェッカーボード / 単色の表示下地を追加した。

**該当**: `crates/ravel-core/src/composition/mod.rs:360`（定義）、
`crates/ravel-app/src/composition_form.rs:66, 116`（編集 UI）、
`crates/ravel-app/src/panels/viewer.rs:1614`（黒 quad のハードコード）

`background_color: Color` は `Composition` のフィールドとして定義され、
コンプ設定フォームで編集でき、`.ravprj` に保存もされる。しかし殻コンパイルにも
評価器にも現れず、**設定しても絵は変わらない**。Viewer は黒 quad を
ハードコードして評価結果を重ねているだけなので、見た目は常に黒背景になる。

帰結が 2 つ:

1. ユーザーは「設定したのに効かない」を踏む（`track_matte` / `time_remap` と
   違い、こちらは UI が既にあるので今日踏める）
2. アルファ 0 の領域と黒の領域を区別する手段が無い。キーイングやマットを
   入れた時点で、透過が正しいかを確認できないまま作業することになる

**修正方針**: 殻合成の最下段でコンプ背景色を敷き、評価結果に含める。
Viewer 側で背景色っぽい quad を描く方式は採らない（書き出しで背景が消え、
Viewer と出力が食い違う）。ハードコードされた黒 quad は撤去する。

**引受先**: `docs/implementation/done/viewer-inspection-plan.md` の `INSP-1`
（チェッカーボード表示と同じ単位。背景の描き方をまとめて扱う）

---

---

> **解決済み**: PR #423（2026-08-13）。`domain` / `source_domain` /
> `target_domain` / `aggregate` / `shape` に `with_param_options` が付き、
> 未知の値は `tracing::warn!` を出してから既定へ落ちる。
>
> 回帰テストは**レジストリの宣言を読み、その各値を文字列パラメータ経由で
> 流す**形（`declared_domain_options_reach_their_attribute_set_domains` ほか）。
> Rust API を直接叩かないのが肝で、それが元の見逃しの原因だった。宣言に
> 無い値が来ると `panic!` するので、選択肢だけ足して分岐を忘れる事故も落ちる。
>
> `attribute.set` の `domain` は Detail を含む 4 値。`style.*` が `point` を
> 外している（`rasterize` が読まないため）のとは事情が違い、素の属性書き込み
> には同じ制限が掛からない。

## MED-CORE-10 | bug | 閉集合の文字列パラメータが dropdown でなく、未知の値を無言で既定へ落とす

**該当**: `crates/ravel-core/src/registry/builtin.rs`（`with_param_options` の欠落）、
`crates/ravel-nodes/src/attribute/mod.rs:208-215`（`domain_param`）、
`crates/ravel-nodes/src/field/mod.rs`（`field.falloff` の `shape`）

`string_parameter` で宣言された 20 個のうち、**閉集合なのに `with_param_options`
が無いものが 5 個**ある。Properties では自由入力のテキスト欄になり、値の解釈は
`match … _ => default` なので、**打ち間違いも未対応の値も無言で既定に落ちる。**

| パラメータ | ノード | 既定への落ち方 |
| --- | --- | --- |
| `domain` | `attribute.set` ほか | `domain_param` に腕が無い値 → 既定 |
| `source_domain` / `target_domain` | `attribute.transfer` | 同じヘルパ |
| `aggregate` | `attribute.promote` | `match … => average` |
| `shape` | `field.falloff` | `match … => sphere` |

**実害が出た実例**: `domain_param` に **`"primitive"` の腕が無かった**ため、
`attribute.set(name = "Cd", domain = "primitive")` が**無言で `point` へ書いて
いた**。`rasterize` はパスの色を Primitive ドメインから引くので、図形は既定色の
まま何のエラーも出ずに描かれる。**既存テストは全て Rust API で `Domain::Primitive`
を直接渡しており、壊れていた文字列パラメータを 1 本も通っていなかった**ので
緑のまま通り抜けた（`every_domain_name_reaches_the_domain_it_names` で回帰を固定済み）。

**閉集合でないもの**（`name` / `group` / `target` / `expression` / `string_value` /
`asset_id` / `port` / `pattern` / `components`）は対象外。`port` と `name` は
文脈依存の候補が要るので `contextual-parameter-options-plan.md`（`CPO-*`）の担当。

**修正方針**は 2 つで 1 組。片方だけでは不十分:

1. **閉集合を宣言する**（`with_param_options`）。UI が不正な値を作れなくなる
2. **未知の値を無言で既定にしない。** dropdown があっても、手編集した `.ravprj`・
   将来のバージョン・パラメータポート駆動から未知の値は来る。最低限
   `tracing::warn!` を出す（`field.attribute` が「列に無い成分」で既にやっている形）

**検証**: **文字列パラメータを通す**形で、宣言された各値がそれぞれの分岐へ届く
テスト（Rust API を直接叩かない — それが今回の見逃しの原因）。未知の値で警告が
出るテスト。

