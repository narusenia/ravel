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

---

## MED-CORE-11 | debt | ジオメトリの bbox の定義がコアと Viewer で食い違う（コアは Point ドメインだけ）

> **解決済み**: `BBOX-1` / `BBOX-3`。「何を測るか」の答えは
> `ops::drawn_bounds` 1 つになった。`GeometricData::bounds` と Viewer の
> `geometry_bounds` は両方ともこれに委譲するので、定義が 2 つある状態自体が
> 消えた。`positions_bounds` は「Point 域の位置の範囲」という部品として残り、
> `drawn_bounds` がその一部として呼ぶ。
>
> 個票の表の「インスタンスだけ」の行は、コアも Viewer も
> **source を置いた矩形**を答えるようになった（インスタンス位置の範囲ではない
> — 位置だけを測るのが `MED-APP-45` の症状だった）。source を持たない
> インスタンス域は置くものが無いので、そのときだけ配置の範囲を答える。
>
> **テスト**: `panels::viewer::geometry::tests::the_core_and_the_viewer_measure_one_rectangle`
> （点だけ / インスタンスだけ / 両方 / 空 の 4 つで同じ矩形）。
> 計画は `docs/implementation/done/geometry-drawn-bounds-plan.md`。

**該当**: `crates/ravel-core/src/geometry/container.rs` の
`Geometry::positions_bounds`（`GeometricData::bounds` の実装）

コアの `positions_bounds` は `Domain::Point` しか見ない:

```rust
fn positions_bounds(&self) -> Option<Rect> {
    let positions = self.positions(Domain::Point)?.ok()?;
    ...
}
```

一方 Viewer の `geometry_bounds`
（`crates/ravel-app/src/panels/viewer/geometry.rs`）は Point と Instance の
両方を走査する。つまり**「このジオメトリの範囲」の答えが 2 つある**:

| ジオメトリ | コアの `bounds()` | Viewer の `geometry_bounds` |
|---|---|---|
| 点だけ | 点の範囲 | 同じ |
| **インスタンスだけ**（`scatter` の出力、`geometry.from_image`） | **0×0**（`positions_bounds` が `None` を返し `bounds()` が既定の 0 矩形へ） | インスタンス位置の範囲 |

インスタンスしか持たないジオメトリはこの系で普通に作られる
（`from_image` は点を 1 つも作らない — `from_image_outputs_one_instance_stamping_the_image`
が `point_count() == 0` を固定している）。

`MED-APP-21`（Viewer の bbox が `type_key` の固定 match で再構成される、解決済み）
と同じ種類の問題で、そのときは「どのノードが描くか」の答えが 2 箇所にあった。
今回は「**何を測るか**」の答えが 2 箇所にある。片方だけ直すと、もう片方が
静かにずれ続ける。

`layer-content-size-plan.md` の「問題 2」で見つけた 3 件のうちの 1 つ。
残りは `MED-APP-45` と `LOW-APP-33`。

## MED-CORE-12 | bug | ドメインパラメータの選択肢を宣言していないノードがあり、タイポが既定値に黙って吸われる

> 起票時は `MED-CORE-10` を名乗っていたが、その番号は
> [closed/medium-core-evaluator.md](../closed/medium-core-evaluator.md) の
> 「閉集合の文字列パラメータが dropdown でない」（解決済み）が既に使っている。
> **同じ監査の取りこぼし分**なので内容は続きだが、ID は別に取り直した（2026-09-28）。

**該当**: `crates/ravel-core/src/registry/builtin.rs`（`attribute.promote` の
`source_domain` / `target_domain`、`attribute.curveu`）

> **解決済み**: `attribute.promote` の `source_domain` / `target_domain` に
> `ATTRIBUTE_DOMAINS` を宣言した。同じ抜けが `field.apply` の `domain` にもあったので
> `FIELD_APPLY_DOMAINS`（`point` / `instance` / `detail`）を宣言した（プロセッサは
> `primitive` を黙って `point` に落とすので、選択肢には出さない）。
> 既存テストは「`domain` / `*_domain` という名前のパラメータを登録簿から全走査して
> 選択肢の宣言を要求する」形に広げた。**起票文の `attribute.curveu` は誤記**で、
> そのノードは `mode` しか持たずドメインパラメータが無い。

`attribute.set` / `attribute.transfer` / `attribute.delete` は
`with_param_options(ATTRIBUTE_DOMAINS)` を宣言しているので、Properties は
選択肢から選ぶ UI になる。**`attribute.promote` の 2 つのドメイン
パラメータと `attribute.curveu` は宣言していない**ので自由入力になり、
綴りを間違えると `domain_param` の警告 + 既定フォールバックで
**黙って別のドメインに書き込む**。

既存テスト（`closed_attribute_parameter_options_match_the_processor_contracts`）
は `promote` の `aggregate` しか見ていないので、この抜けを捕まえない。

**実害**: 小さいが黙っている。プロジェクトを保存すると誤ったドメイン名が
そのまま残り、開き直しても同じ既定に吸われ続ける。

**修正方針**: 3 パラメータに `with_param_options` を足し、既存テストの
走査対象を「ドメインを取る全パラメータ」に広げる。

**severity の根拠**: bug（誤った入力が無言で別の意味になる）。low でないのは
永続化された値が黙って別解釈されるため、high でないのは誤りが 1 ノードに
閉じ、データが壊れないため。

---

## MED-CORE-13 | bug | 属性列の連結が欠けた側を型ゼロで埋めるので、「無い」を既定値と読む予約属性が消える・透明になる

> **解決済み**: #591（`FILL-1`〜`5`）と #592（`FILL-6`、仕様・API 地図・クローズ）。
> `ravel-core` の `geometry::absent` が予約属性の「無いときの値」の正になった
> （定数は `alpha` 1 / Instance `scale` `(1, 1)` / `pscale` 2 / Instance `Cd` 白、
> `fill` / `stroke_width` / Primitive・Point の `Cd` / `stroke_color` は継承なので
> `rasterize` テンプレートの既定で実体化し、`stroke_color` は同じ側の `Cd`、Point の
> `Cd` は所属プリミティブの線色から行ごとに写す）。`geometry.merge` / `expand_instances` /
> `attach_piece_attributes` / `field.apply` の作成は欠けた行をこれで埋め、予約されていない
> 列と予約名に予約外の型が載った列は型ゼロのまま。`style` の `UNSET_*` と `rasterize` の既定の
> 直書きは `absent` の定数を参照する（挙動は変えない）。`field.apply` の作成は、不在値が
> float・ベクタ・色の予約名を宣言の型で作り、I32 / Bool（`source_index` / `stroke_align`）は
> 従来どおりフィールドの型で作る。`sources_with_different_columns_fill_with_typed_zeros` と
> `merge_unions_attributes_with_typed_zero_fill` の `pscale` の期待値は 2.0 に直した。
> 既知の限界: 写した `stroke_color` は、下流で `Cd` を変調しても追随しない
> （「無い」なら追随した）。

**該当**: `crates/ravel-nodes/src/geometry.rs:397-466`（`geometry.merge` の
`concat_attribute_sets` / `concat_columns`）、`crates/ravel-core/src/geometry/ops.rs:2759-2884`
（`expand_instances` の `ColumnAccumulator` / `append_rows`）、同 `:2604-2653`
（`attach_piece_attributes`）、`crates/ravel-core/src/geometry/field.rs:1840-1865`
（`field.apply` の `created_column`）

列の連結で片側に列が無いとき、その行を**列の型のゼロ**（`0.0`、`(0, 0)`、
`Color::TRANSPARENT`、`false`）で埋める。一方で読み手（主に `rasterize`）は、
予約属性の列が**無い**ことを別の値として読む。

| 属性 | 無いときの読み | ゼロ埋めの結果 |
|---|---|---|
| `scale`（Instance） | `(1, 1)`（`InstanceColumns::placement`、`container.rs:575-590`） | `(0, 0)` でインスタンスが潰れる |
| `alpha` | `1.0`（`rasterize/mod.rs:2056-2058`、`:859-860`） | `0.0` で透明 |
| `Cd`（Instance） | 白（掛け算の色味。`rasterize/mod.rs:1282-1287`） | 透明で消える |
| `Cd`（Point / Primitive） | `rasterize` の `color`（`element_color`、`:2052-2054`） | 透明で消える |
| `pscale` | `DEFAULT_POINT_RADIUS = 2.0`（`:96`、`:1234`、`:1905`） | 半径 0 で描かれない |
| `fill` | `rasterize` の `fill`（既定 `true`。`element_style`、`:2063-2072`） | `false` で塗られない |
| `stroke_color` | 自分の塗り色（`Cd`）へフォールバック（`:2077-2083`） | 透明な線 |

**再現**（コードで追った経路。いずれも組み込みノードだけで作れる）:

1. `style.fill`（赤）を掛けたパス A と、何も掛けていないパス B を
   `geometry.merge` → `rasterize`。B の Primitive 行は `fill = false`、
   `Cd = (0, 0, 0, 0)` で埋まり、**B が描かれない**（期待: `rasterize` の
   `fill` / `color` で塗られる）
2. `geometry.from_image` の出力を、そのままの枝と `geometry.transform`
   （`scale = 2`）を掛けた枝に分けて `geometry.merge`。ソースは同じ `Arc` なので
   マージは通り、変換した側だけが `scale` 列を持つ（`geometry.rs:212-226` は値が
   変わるときだけ書く）。**変換していない側の画像が `scale = (0, 0)` で消える**
3. パス A の Point ドメインに `field.ramp` で `Cd` を書き（線の頂点色）、
   素のパス B と `geometry.merge`。B の頂点は `Cd = 透明` の頂点色を持つので
   **B の線が透明になる**（期待: B のプリミティブの色の線）
4. 図形 + 文字を `geometry.merge` し、文字のインスタンスに `field.apply` で
   `alpha` / `Cd` を書いてから `text.to_path`。インスタンス列は展開で各グリフの
   Point / Primitive へ降りるが、ホスト自身の図形のブロックは列を持たないので
   **ホストの図形が `alpha = 0` / `Cd = 透明`で消える**
5. `field.apply` を `alpha` に `combine = multiply` で掛ける（列が無いので
   `create_if_missing` が作る）。作られる列は `0.0` なので、掛けた結果も 0 で
   **ジオメトリが透明になる**。`Cd` だけは `created_column` が白で作るよう
   直してあるが、`alpha` / `scale` / `pscale` は型ゼロのまま

**テストが欠陥を固定している**: `sources_with_different_columns_fill_with_typed_zeros`
（`ops.rs:5584`）は `pscale` の欠けを `0.0` と、`merge_unions_attributes_with_typed_zero_fill`
（`geometry.rs:1821`）は `[.., 0.0, 0.0]` と期待値に書いている。どちらの
フィクスチャも `pscale` を持たない点がパスの頂点で、`rasterize` はパスの頂点を
スプライトとして描かない（`path_vertex_mask`）ので**絵には出ない**。同じ規則を
パスに属さない点に当てると半径 2.0 の点が消える。`a_piece_without_a_column_fills_with_the_typed_zero`
（`scatter/mod.rs:1488`）も規則そのものを固定している（列は `char_index` で、こちらは
予約属性の既定値を持たないので結果は正しい）。

**原因**: 「列が無いときの値」の正が無い。`rasterize` の `unwrap_or`、
`InstanceColumns` の恒等配置、`style.rs:26-33` の `UNSET_*`、`field.rs` の
`created_column` がそれぞれ自前で持ち、連結の 3 箇所は型ゼロしか知らない。
しかも `fill` / `stroke_width` / Point・Primitive の `Cd` / `stroke_color` の
「無い」は定数ではなく**`rasterize` のパラメータや囲むインスタンスから継承する**
という意味で、密な列はその「意見なし」を持てない（`style.rs:45-52` が同じ理由で
group の種まきを拒否している）。

**影響**: スタイル済みと未スタイルのジオメトリ、変換済みと未変換のインスタンスを
マージするという普通の操作で、片側が黙って消える。エラーも警告も出ない。
永続化されるのはノードのパラメータだけ（ジオメトリは直列化されない）ので、
修正で壊れるデータは無い。

**修正方針**: 予約属性について「列が無いときの値」を ravel-core に 1 つ置き、
連結の 3 箇所・`field.apply` の作成・`style` の `unset`・`rasterize` の読みを
すべてそこへ寄せる。予約されていない列は型ゼロのまま。継承する属性の扱いは
利用者の判断が要る。計画は `docs/implementation/done/absent-attribute-fill-plan.md`。

**severity の根拠**: bug（出力そのものが誤り、かつ無言）。high に近いが、
片側に列が無いマージ・展開という条件を踏んだときだけで、データは壊れないので medium。
