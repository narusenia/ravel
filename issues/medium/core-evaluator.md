# medium — ravel-core（評価器・ジオメトリ・undo）

深刻度 medium の課題を領域単位でまとめる。各項目は独立して着手可能。

> **例外**: `MED-CORE-02` / `03` / `06` / `07` は
> `docs/implementation/cache-plan.md` が引き受ける（それぞれ CACHE-4 / CACHE-2 /
> CACHE-3 / CACHE-3）。4 件すべてがキャッシュの同一性・予算・無効化という
> 同じ関数群を書き換えるので、**個別に直すと衝突する**。

---

## MED-CORE-04 | bug | 評価とサブネット再帰走査に深さ上限が無い — 深いグラフでスタックオーバーフロー

**該当**: `crates/ravel-core/src/eval.rs:840-1178`

> **一部のみ解決（2026-08-03 再判定）**: 評価とサブネット**再帰走査**には
> `EvalError::DepthLimitExceeded { node, limit }` が入り（`eval.rs:106`, `:2000`,
> `:2546`, `:2601`）、ロード後の検証にも `Document::validate_subnet_depth`
> （`composition/mod.rs:829`、上限 `MAX_SUBNET_DEPTH = 64`）が入った。
>
> **だがデシリアライズ経路は依然として無防備。** `ProjectFile::from_archive` は
> `ron::from_str::<Document>(text)`（`crates/ravel-project/src/lib.rs:286`）を
> **`validate_subnet_depth()`（`:289`）より先に**実行する。
> `Node.subnet: Option<Arc<Graph>>`（`graph.rs:363`）は再帰的なので、深くネストした
> サブネットを持つ細工済み / 破損した `.ravprj` は**パース中に**スタックを消費して
> abort しうる。本項目の記述（下記「`Graph` のデシリアライズ」「`Document::validate`
> を迂回してロード時にクラッシュ」）がまさにこの経路を指しているため、未解決に戻した。
>
> 残作業: 深さ制限付きデシリアライズ（`serde` の再帰深度制限、または
> パース前のテキスト走査による事前拒否）。

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


---

## MED-CORE-09 | bug | 閉集合の文字列パラメータが dropdown でなく、未知の値を無言で既定へ落とす

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
