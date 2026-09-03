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
> （`composition/mod.rs`、上限 `MAX_SUBNET_DEPTH`。`HIGH-26` で 64 → 16）が入った。
>
> **デシリアライズ経路は `HIGH-26` の修正で閉じた（2026-08-20 再判定）。**
> RON リーダは全経路が `composition::RON_RECURSION_LIMIT`（192）を使うようになり、
> 予算を**超えた入力はパース中にエラーを返す**（スタックを消費し切らない。
> 実測で RON 360 段は 2 MiB スタックに収まり、464 段で溢れる）。上限は
> `MAX_SUBNET_DEPTH`（**64 → 16**）が要求する段数の上に置かれ、保存側にも
> 深さ検査が入ったので「保存できたが開けない」も消えた。詳細は
> [`../closed/HIGH-26-ravprj-saves-deeper-than-it-loads.md`](../closed/HIGH-26-ravprj-saves-deeper-than-it-loads.md)
> と `docs/dev/persistence.md` の「保存できたものは開ける」。
>
> **残っているのは評価側**（下記の `eval_node` / `pull_input` の再帰と、
> ロード時 `normalize_*` の再帰走査）。数千ノードの直線チェーンは
> `MAX_EVALUATION_DEPTH` で拒否されるが、再帰そのものは明示的な
> ワークスタックになっていない。この項目はそのために未解決のまま残す。

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

## MED-CORE-09 | debt | テンプレートとプロセッサの対応を機械的に確かめるテストが無いので、登録忘れは「置けるが評価されない」ノードになる

**該当**: `crates/ravel-nodes/src/lib.rs`（`processor_for_node` の巨大な
`match`）、`crates/ravel-core/src/registry/builtin.rs`（`register_builtins`）

ノードを足すには 2 か所に書く必要がある — レジストリに `NodeTemplate` を、
`processor_for_node` に `type_key` の腕を。**片方を忘れてもコンパイルは通る**。
テンプレートだけ足すと、Add Node に出て配線もできるが評価だけが `None` を
返すノードができる。`docs/dev/add-node.md` は「忘れやすいもの」として
名指ししているが、**コード側のガードは無い**。

同種の抜けは他の資産では塞がれている: ロケールは
`all_builtin_templates_have_a_label_in_every_locale` が全テンプレートを走査し、
アイコンは `every_node_template_icon_is_embedded` が走査し、テンプレート数は
`register_all_builtins` が数える。**評価経路だけが走査されていない。**

**実害**: 中程度。壊れ方は「ノードを置いたのに何も出ない」で、原因が
レジストリと処理系の食い違いだと分かるまで時間を食う。データは壊れない。

**なぜ素朴なテストが書けないか**: `processor_for_node` は `GpuContext` を
要求するので、「全テンプレートを回して `Some` が返ることを確かめる」テストは
GPU アダプタが無い環境で通らない。

**修正方針**: アダプタを要らない形にする。案としては、(a) `type_key` の
集合を宣言的に持って両側から突き合わせる、(b) プロセッサ構築を GPU 依存から
切り離して「構築できるか」だけを問える関数を出す、(c) アダプタ必須の
スイープテストにして CI の GPU 有無で skip する。**(a) が最も安い**
（`processor_for_node` の `match` の腕を配列から生成するか、逆に配列を
match から導く）。

**severity の根拠**: bug ではなく debt（現在の登録は全部揃っている）。
low でないのは、`docs/dev/add-node.md` 自身が警告している穴が
**ノードを足すたびに踏まれる可能性を持ち続ける**ため。2026-09-03 の
`MOD-3` / `MOD-4` / `OPS-2` の実装で**3 回独立に指摘された**。

## MED-CORE-10 | bug | ドメインパラメータの選択肢を宣言していないノードがあり、タイポが既定値に黙って吸われる

**該当**: `crates/ravel-core/src/registry/builtin.rs`（`attribute.promote` の
`source_domain` / `target_domain`、`attribute.curveu`）

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

