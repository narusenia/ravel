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

---

## MED-CORE-13 | bug | 属性列の連結が欠けた側を型ゼロで埋めるので、「無い」を既定値と読む予約属性が消える・透明になる

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
利用者の判断が要る。計画は `docs/implementation/absent-attribute-fill-plan.md`。

**severity の根拠**: bug（出力そのものが誤り、かつ無言）。high に近いが、
片側に列が無いマージ・展開という条件を踏んだときだけで、データは壊れないので medium。
