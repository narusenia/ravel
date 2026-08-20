# 永続化を変更する

> 索引: [`README.md`](README.md)

`.ravprj` の構造とフォーマットバージョンの扱い。データモデルの定義は
[`../specifications/data-model.md`](../specifications/data-model.md)。

## `.ravprj` の中身

| エントリ | 内容 |
|---|---|
| `manifest.json` | `format_version` とプロジェクト情報。**マイグレーション連鎖の起点** |
| `document/main.ron` | Composition・レイヤー・ネットワーク（Subnet 入れ子含む）・キーフレーム・`media_assets`（v9 で `AssetId` キー）・`exposed_parameters`（公開パラメータ宣言、v7 で追加）。決定的 RON |
| `settings.toml` | プロジェクト設定 |
| `ui_state.json` | UI 状態（アクティブコンポジション、Timeline の BPM グリッド、コンポジションごとのループ範囲）。**任意エントリ**で、欠落時はアクティブコンポジションが `root_comp` に、BPM グリッドが既定に、ループ範囲が 0 件にフォールバック |
| `workspace_layout.toml` | ワークスペースレイアウト。**任意エントリ**かつ**オプトイン**（既定 OFF）で、トグルが OFF のときは書かれない |

保存時は前のリビジョンを `.bak` にする。書き込みはアトミック
（`CRIT-03` で修正済み）。

`.ravprj` の外にもう 2 つ永続化先がある。`<config>/ravel/layout.toml` が
**アプリレベルのレイアウト**（全窓のツリー・`WindowPlacement`・AlwaysOnTop・
名前付きレイアウト）で、`settings.toml` の 4 層マージとは独立。中身の型は
`.ravprj` 側の埋込エントリと同じ `ravel_ui::layout_doc::LayoutDocument` で、
どちらも `layout_version` を持つ。実装は `crates/ravel-app/src/layout_persist.rs`。
もう 1 つが `<config>/ravel/settings.toml`（**global 設定層**）で、下記。

## 設定の永続化

設定は 2 つの層が別のファイルに載る。**書き込みは層ごとに独立**で、片方を
書いてもう片方のファイルを触ることはない（実装 =
`crates/ravel-app/src/app_settings.rs`）。

| 層 | 置き場 | 書くタイミング |
|---|---|---|
| `global` | `<config>/ravel/settings.toml` | 変更ごとに即時。アトミック書き込み、UI スレッド外 |
| `project` | `.ravprj` の `settings.toml` | 次のプロジェクト保存。変更で project が dirty になる |

- **壊れた設定ファイルで起動を止めない。** 欠落・読めない・パースできないの
  どれでも「上書き無し」に倒し（欠落は無ログ、それ以外は警告）、既定で起動する。
  レイアウトと同じ方針
- **1 項目更新のみ。** `app_settings::update(scope, |layer| …, cx)` は編集した
  層だけを永続化する。層を丸ごと差し替える API は置かない（新しいフィールドを
  知らない呼び出し元が他の上書きを消せてしまう）。`Option` を `None` にすると
  その層から値が消え、下位層の値に戻る（「既定に戻す」）
- **保存失敗を無言にしない。** global 層の書き込み失敗は
  `ProjectEvent::SettingsSaveFailed` として通知に出る（`CRIT-02` と同じ失敗形を
  作らないため）
- **アトミック書き込みは共有する。** `crates/ravel-project/src/atomic_write.rs`
  が temp ファイル + sync + 名前入れ替えの 1 実装で、`.ravprj` の書き込みも
  これを使う（Windows の置換プリミティブを 2 箇所に持たないため）
- **読むのは GUI だけではない。** `ravel-cli render` も `global` + `project` を
  解決してキャッシュ予算に流す（`render_with_hooks` の `global_settings` 引数が
  global 層のファイルを指す）。だから設定値の検証は
  `crates/ravel-project/src/settings.rs`（`cache_limit_mb` /
  `cache_sim_reserve_ratio` / `cache_root_setting` / `usable_cache_budget`）に
  置く。`ravel-app` にあると CLI から呼べず、範囲のコピーが 2 つになる
- project 層は**ドキュメント差し替えの経路だけ**が入れ替える
  （`app_settings::set_project_layer`）。開いたプロジェクトの上書きが開いた
  瞬間から効き、閉じた瞬間に効かなくなる

## レイアウトの永続化

- **`layout_version` を上げるのは既存フィールドの意味・型が変わるときだけ。**
  レイアウトは移行連鎖を持たない。読めない・バージョンが未知・構造が不正の
  どれでも**既定レイアウトに倒す**（`LayoutDocument::from_toml` はエラーを返し、
  呼び出し側が必ずフォールバックする）。起動を妨げてはならない
- **`.ravprj` 側の埋込はセッション限定。** 開いたときのレイアウトには使うが、
  `LayoutStore::capture` がアプリレベルの既定へ書き戻すのを拒否する。埋込あり /
  なしのプロジェクトを交互に開いても `layout.toml` は汚れない
- 埋込プロジェクトの次に埋込なしプロジェクトを開くと、**アプリレベルの
  レイアウトへ戻る**（他人のレイアウトがプロジェクトより長生きしないように）
- レイアウトを外から入れる経路は `WorkspaceLayout::adopt` の 1 本だけ。メイン窓の
  論理 ID を維持し、インスタンス ID を振り直す（キャッシュ済みペインビューが
  別種のパネルに渡らないようにするため）

## サブグラフテンプレートの永続化 (`*.ravtpl`)

サブネットの内部グラフと、そのサブネット内に束縛された公開パラメータ宣言を
1 ファイルにまとめたもの（実装 =
`crates/ravel-project/src/subgraph_template.rs`、型は
`ravel-core::subgraph_template::SubgraphTemplate`）。

| 置き場 | 形式 | 書くタイミング |
|---|---|---|
| `<config>/ravel/subgraph-templates/*.ravtpl` | 決定的 RON（`struct_names`、2 スペース） | 新規は `save_new`（`create_new`）、上書きは `replace`（アトミック書き込み） |

- **`.ravprj` のフォーマットは上げない。** テンプレートはプロジェクトの外の
  ファイルで、貼り付けたときに宣言が入るのは v7 で既にある
  `Document.exposed_parameters`。ユーザーが配って落として使う形はキーバインド
  上書き（`<config>/ravel/keybindings.toml`）と同じ立ち位置
- **宣言の検証はプロジェクトと共有する。** ファイルの `declarations` は
  `ExposedParameters` そのものなので、不変条件を破る宣言だけが落ち
  （警告付き）、テンプレートの残りは読める。**テンプレート側に別の検査を
  置かない** — 置いた瞬間に 2 つの契約が食い違い始める
- **1 つ壊れても図書館ごと失わない。** `load_dir` は読めないファイルを警告して
  飛ばし、ディレクトリが無ければ「テンプレート 0 件」を返す（エラーにしない）
- 貼り付けは ID を振り直す（`SubgraphTemplate::instantiate`）。**宣言の束縛も
  同じ対応表で書き換える** — 書き換え漏れは「解決しない」ではなく「別の
  インスタンスを動かす」形で壊れるので、ノードとセットで 1 コミットに入れる。
  **書き換えられない束縛（内側グラフに無いノード）は貼り付けを失敗させる** —
  `NodeId` はただの整数で、貼り付け先が同じ ID の無関係なノードを持っていたら
  それを掴む。ファイルの読み込み自体は通す
- **保存はパスでなく名前で行う。** `save_new` / `replace` が
  `<ライブラリ>/<検査済み 1 成分>.ravtpl` を組み立てる（区切り文字・`..`・
  Windows のドライブ記号・先頭 `.` を拒否）。ユーザーの入力をそのまま `Path`
  として渡す形が `.ravprj` の上書きになる。新規と上書きは別 API。
  `load_dir` はシンボリックリンクを辿らない
- **入れ子の深さは `.ravprj` と同じ上限**（`MAX_SUBNET_DEPTH`、判定は
  `composition::subnet_depth_exceeds` の 1 本）。`from_ron` と `instantiate` で
  掛ける。RON の再帰上限は `RON_RECURSION_LIMIT` でこの上限の上に置いてあるので、
  **落とすのは深さの検査のほう**（下の「保存できたものは開ける」）

## 不変条件: 保存できたものは開ける

**書き手が受け付けた文書は、同じビルドが読み直せなければならない。**
これが破れると、ユーザーには「保存は成功したのに二度と開かない」形で出る
（`HIGH-26`。実質的なデータ損失で、保存した時点では気づけない）。

`.ravprj` / `.ravtpl` / undo ジャーナルの RON にはこれを支える 2 つの数字がある。

| 定数 | 置き場 | 役目 |
|---|---|---|
| `MAX_SUBNET_DEPTH` | `ravel-core::composition` | 文書が許すサブネットの入れ子段数。**書き手側の唯一の上限** |
| `RON_RECURSION_LIMIT` | 同じ場所 | RON デシリアライザの再帰予算。`MAX_SUBNET_DEPTH` が要求する段数の**上**に置く |

守る手順:

- **RON を読む経路は全部 `RON_RECURSION_LIMIT` を使う。** `ron::from_str` を
  素で呼ぶと RON 既定の 128 段になり、その経路だけが上限違いになる
  （`ravel-project` は `ron_options()` 1 本、`ravel-core` の
  `undo::journal::RonCodec` も同じ定数を引く）
- **保存側は書く前に `Document::validate_subnet_depth` を通す**
  （`ProjectFile::to_archive_for_root`）。シリアライザ自身には再帰上限が無いので、
  検査しなければ「読めないファイル」を書けてしまう
- **`.ravtpl` も同じ**（`subgraph_template::save_new` / `replace` が
  `SubgraphTemplate::check_nesting` をファイル作成前に通す）。`capture` は
  文書側の上限を見ないので、深いサブネットからテンプレートを作れてしまう
- **`MAX_SUBNET_DEPTH` を上げるときは `RON_RECURSION_LIMIT` も上げる。**
  サブネット 1 段は RON の約 8 段（レイヤーネットワーク経路の実測）。
  ただし**再帰予算はスタックの上限でもある**: 実測でサブネット 64 段 =
  RON 464 段が 2 MiB のスレッドスタックを溢れさせた。だから
  「読めるように予算を上げる」には天井がある
- `ravel-project` の `a_document_nested_to_the_limit_survives_a_save_and_a_load`
  と `a_document_nested_past_the_limit_is_refused_by_the_save` が、上限の
  両側をテストで固定している。**上限を動かしたらこの 2 本が落ちる**

## 判断: バージョンを上げるか、上げないか

```text
既存フィールドの意味・型・単位を変える     → バージョンを上げてマイグレーションを書く
新しいフィールドを足すだけ                → 上げない。#[serde(default)] で読む
新しい任意エントリを足すだけ              → 上げない
外部が名前で消費する契約を足す            → 上げる（旧ビルドの黙った消失を防ぐため）
```

最後の行だけが「追加フィールドでも上げる」例外で、**v7 の
`Document.exposed_parameters`（公開パラメータ宣言、REQ-PROJ-006）**がそれ。
旧ビルドは知らないフィールドを黙って捨て、次の保存でそれ抜きの文書を書き戻す。
`Layer.audio` ならユーザーが音の消失に気づくが、宣言は**別のツールが名前で
読む契約**なので、消えても画面上は何も変わらない。版を上げておけば旧ビルドは
`MigrationError::TooNew` で開くのを拒否する。判断基準は「フィールドか否か」では
なく「**黙って消えたときに気づけるか**」。

`ParameterValue` に variant を足すのは別枠。**必ず末尾に足し**、
`JOURNAL_FORMAT_VERSION`（`ravel_core::undo::journal`）を上げる。bincode は
variant を位置で索引するので途中挿入は旧 journal を壊す。`.ravprj` の
`format_version` は、既存パラメータの表現を変えるとき（v6 のカーブ）だけ
上げる — variant を足すだけなら上げない。

**追加フィールドでバージョンを上げない**のは前例がある: `Layer.audio` は
format v4 のまま `#[serde(default)]` の追加フィールドとして入り、
マイグレーションも作っていない。`Composition.guides`（ユーザーガイド、
`SNAP-2`）も v8 のまま同じ形で入っている — 欠落は「ガイドが無い」という事実
そのもので、旧ビルドが捨てても画面から線が消えるので気づける。
`ui_state.json` も format_version 3 のまま追加された。

この判断は `docs/implementation/roadmap.md` の基準 1（移行コストが時間で
増える単位を先に）にも影響する。**フィールド追加だけなら後回しのコストは
上がらない**ので、基準 1 を根拠に前倒しする理由にはならない。

## マイグレーションを書く

`crates/ravel-project/src/migration.rs`。`migrate_to_current` が
`format_version` を見て `migrate_vN_to_vN+1` を順に適用する連鎖。

- 1 段ずつ書く（v3 から v5 へ直接飛ばさない）
- 対象は `serde_json::Value`（型に落とす前）。型を変えている最中なので
  構造体経由では書けない
- **旧ファイルを 1 つでも読めなくしない。** 連鎖の各段にテストを置く
- ドキュメント本体（RON）の構造変更は、読み込み側で `#[serde(default)]` と
  variant 追加で吸収できるかを先に検討する

### この連鎖は `document/main.ron` を見ない

`migration.rs` が触るのは `manifest.json` だけ。ドキュメント本体は
`Document` へ**型付きでデシリアライズ**されるので、RON の中身を変える移行は
**ロード後のドキュメントに対する型付きパス**として書き、`format_version` で
ゲートする。

前例が 4 つある。

**v4 → v5 のベクタパラメータ畳み込み**
（`ravel_core::composition::Document::fold_component_params`、実装計画は
[`../implementation/vector-field-plan.md`](../implementation/vector-field-plan.md)
の単位 5）。ノードのパラメータは自由なキー / 値の対なので、旧ファイルの
`center_x: Float(..)` はテンプレート宣言と一致しなくても RON パースを通り、
黙って読まれなくなるだけ。だから `migrate_v4_to_v5` はバージョン印だけを
進め、畳み込みは `ProjectFile::from_archive` が
`source_version < 5` のときに実行する。

**v5 → v6 のカーブパラメータ変換**
（`Document::upgrade_curve_params`、実装計画は
[`../implementation/properties-parameter-editors-plan.md`](../implementation/properties-parameter-editors-plan.md)
の単位 1）。`field.curve_remap` の `points` は `"0:0,1:1"` 文字列だったのを
`ParameterValue::Curve` に変えた。理由も形も v5 と同じで、`migrate_v5_to_v6` は
版印だけを進め、変換は `source_version < 6` のときに走る。

**v7 → v8 の色のリニア化**（`Document::linearize_colors`、実装計画は
[`../implementation/color-management-plan.md`](../implementation/color-management-plan.md)
の `CM-2`）。パイプラインがリニアになったので、作者が指定した色の**意味**が
変わった。`migrate_v7_to_v8` は版印だけを進め、読み替えは
`source_version < 8` のときに走る。色を持つ永続化を触るなら次の 4 点が要る。

- **色かどうかは値から判別できない。** `Channel4` は `constant.color` では色、
  `attribute.set` の `type = "vec4"` では単なるベクタで、保存形は同一。
  ノードテンプレートのポート宣言型（`COLOR` か `VEC3` / `VEC4` か）で決める。
  **テンプレートが引けないノードは変換せず報告する**
- **アルファは変換しない**
- **グラフの外にも作者指定の色がある。** コンポジションの背景色と
  `exposed_parameters` の `color` 既定値は、ノードを歩く walk では見つからない
- **この変換は冪等ではない。** `srgb → linear` を二度かければ別の色になる。
  一度だけにするのは**バージョン印**の仕事であって、値の検査ではできない。
  だから追加のフィールドではなくバージョンを上げる
- **既知の上限**: 版印が壊れて**有効な v7 に化けた** v8 の書庫は、二重に
  リニア化されても検出できない。版印が欠けていれば `MissingVersion`、
  数値として読めなければ `InvalidVersion` で読み込みが止まるので、
  すり抜けるのはこの場合だけ。値からは判別できないため、防ぐには
  書庫の完全性検査（チェックサム）が要る

変換できなかった箇所（式で駆動される色、キーフレーム間の補間のずれ）は
`ColorMigrationReport` に集計して返し、ロード後に警告として出す。**黙って
値を変えるより、変わらなかったことを伝える。**

**v8 → v9 の素材 ID 化**（`Document::upgrade_asset_references`、実装計画は
[`../implementation/asset-identity-plan.md`](../implementation/asset-identity-plan.md)
の `AID-1` / `AID-2`）。v8 までは**素材の表示名がそのまま同一性**で、
`Document::media_assets` のキーと参照 3 系統（`media` ノードの `asset_id`
パラメータ・`AudioSource`・公開パラメータ宣言が束縛する `media` ノード）が
同じ文字列を持っていた。v9 はキーを `AssetId` にし、文字列を
`MediaAssetEntry::name` に移す。`migrate_v8_to_v9` は版印だけを進め、
張り替えは `source_version < 9` のときに走る。

- **旧キーは deserialize でしか見えない。** 参照は `Document` の中で
  `compositions` → `media_assets` の順に読まれるので、どちらが先に旧文字列に
  出会うか決まらない。だから `AssetId` の `Deserialize` が文字列を受け付けて
  ID を採番し、対応表を `composition::asset_legacy` の**スコープ付き
  thread-local** に残す。`asset_legacy::scoped` が 1 回のデシリアライズを
  囲み、表を返す。表を**必ず捨てる**こと — 同じ名前を持つ 2 つの旧文書が
  ID を共有すると、片方から貼ったレイヤーが**もう片方のファイルに繋がる**
- **型付きパスが直すのは `media` ノードのパラメータだけ。**
  `AudioSource::asset_id` は `AssetId` なので上の interning が読み込み中に
  解決済みで、素材表の同名エントリと同じ ID になる。`media` ノードの参照は
  型無しの `ParameterValue::String` で interning の目に入らないので、
  こちらだけ名前で引き直す
- **`AssetId` の `Deserialize` は 4 通りの綴りを受ける。** `deserialize_any`
  を使う以上、隣の ID 型が derive で得ている newtype の綴りは自前で扱う:
  RON は newtype を `(1)`（`struct_names` 時は `AssetId(1)`）と書くので、
  整数・1 要素シーケンス・newtype・旧文字列の 4 つが同じ ID になる
- **解決できない参照は文字列で残さず `AssetId::UNSET` にする。** 旧文書は
  「素材表に無い名前を指す参照」を既に持ち得る（v8 までの削除がそれを残した）。
  名前のまま残すと、次に同じ名前でインポートされた瞬間に**別のファイルへ
  繋がり直す** — v9 が消しに来たバグそのもの。`media` ノードは
  解決できない ID をエラーにせず**オフライン扱いで透明フレーム**を返す
  （v9 で挙動を反転させた点。それまでは未知の ID は評価エラーだった）
- **版を上げる理由は「1 度だけ」。** 文字列キーは形から見分けられるので
  データ駆動でも走らせられるが、上の張り替えは**意図的に不可逆**。v9 文書の
  `name` は編集可能で重複してよいので、そこへ同じパスを掛けると生きている
  参照をオフラインにしてしまう

**逆に v6 → v7（`Document.exposed_parameters`、公開パラメータ宣言）は型付き
パスを持たない。** 版を上げた理由は上の判断表のとおりだが、**変換すべき既存の
表現が無い**ので `migrate_v6_to_v7` も `from_archive` も版印を進めるだけで終わる。
`#[serde(default)]` により v6 文書は「宣言ゼロ」— それが事実そのもの — として
読める。`source_version < 7` の分岐を書きたくなったら v4 → v5 / v5 → v6 との
違いを確認すること: あの 2 つは**既存の値の表現**が変わったので、読んだ文書を
書き換えなければ同じ意味にならなかった。

型付きパスを書くときの注意:

- **全グラフを走査する**: 平坦グラフ、各 `Layer::network`、`Node::subnet` の内側。
  走査そのものは共有する — 文書側が `Document::map_graphs`、入れ子側が
  `composition::graph_walk::map_subnets`。新しい移行は 1 グラフの書き換えだけを
  書き、走査は使い回す
- **冪等にする**: 2 度走っても同じ結果になること（保存後の再ロードで走らない
  ことに依存しない）
- **ID を発行するなら `advance_id_counters()` の後に走らせる**。畳み込みは
  露出済みパラメータポートを保存するために `vector.construct` ノードを挿入する
  ので、`NodeId::next()` が文書内 ID と衝突しない位置で実行する必要がある
- **保存できないものは黙って壊さず落とす**。畳み込み先のポートが受け入れる
  wire 型を出す `vector.construct` が無い場合、エッジは `tracing::warn!` を
  出して落とす。値そのものは畳んだパラメータに残る。4 成分パラメータの
  ポートは `COLOR` と `VEC4` の両方を受けるので（`port_accepted_types`）
  `vector.construct.vec4` で救える — 落ちるのは、そのノードの型が読まない
  余剰キー（`type = "vec2"` のときの `value_z` など）へのエッジ
- **旧リーダーの規則を読み直してから書く。** 移行の正しさは「新しい表現として
  妥当か」ではなく「**旧実装と同じ値を返すか**」で決まる。カーブ変換の旧実装は
  `parse_curve`（`filter_map` で壊れた要素だけ捨てる）→ `CurveRemapField::new`
  （評価前にソート）→ `remap_curve` の 3 段で、この 3 つを合わせた挙動が仕様。
  **旧実装をテストに写して掃引で突き合わせる**のが確実（`curve_upgrade.rs` の
  `v5_remap`）。正常系だけでなく、壊れた入力も掃引に入れる
- **読めない部分だけ捨て、全滅のときだけ既定へ倒す。**「1 つでも壊れていたら
  既定に戻す」は安全に見えて、部分的に壊れた旧ファイルの**描画結果を静かに
  変える**。カーブ変換は読めない要素を 1 つずつ捨て、読める点が 0 個のときだけ
  `CurveParam::identity()`（0:0, 1:1）にする。捨てた個数は `tracing::warn!` に
  載せる
- **新しい型の不変条件はデシリアライズでも守る。** `.ravprj` はテキストなので
  手編集・マージ・切り詰めが起こる。`derive(Deserialize)` はコンストラクタを
  通らないため、`CurveParam` は独自の `Deserialize` で非有限値を落とし、
  並べ替え、重複入力を畳む。**壊れた入力でロードを失敗させない**のが方針。
  `ExposedParameters`（公開パラメータ宣言）も同じ形で、名前の重複と
  「既定値の型が宣言の型と食い違う宣言」を**その宣言だけ落として**読む
  （名前が重複したら**先に書かれた方が残る** — ファイルの順序が呼び出し側に
  見せている契約だから）。落とした宣言は `tracing::warn!` に出す:
  外部契約の一部が黙って消えてよいわけではない。
  **どちらの層が寛容かを決めること。** 単体の `ExposedParameter` を読むのは
  厳格（不整合なら serde エラー）で、寛容さは「残りを活かせる」集合の層だけが
  持つ
- **消せない差は文書に書く。** カーブ変換に残る差は 2 つ（非有限値の点を落とす、
  重複入力の段差を後の点に畳む）。どちらも Ravel が書かない入力に限られるが、
  コードのモジュールコメントと実装計画の両方に明記する

## ID の扱い

- ロード時に `NodeId` / `EdgeId` / `CompId` / `LayerId` / `AssetId` のカウンタを
  ドキュメント最大 ID より先へ進める（REQ-LAYER-009）。新しい ID 種を足したら
  ここも足す
- **水位は参照側も走査する。** `layer.ref` のターゲットと同じ理由で、
  `id_watermarks` は素材表のキーだけでなく `media` ノードの `asset_id` と
  `AudioSource` も見る。素材表に無い `AssetId` を指す参照は v9 では**正常な
  状態**（オフライン）なので、そこを走査しないと次の採番がその番号に当たり、
  オフラインだった参照が無関係なインポートに**繋がり直す**
- `type_key` とパネルの `panel_id()` は**永続化に載る文字列**。後から変えると
  それぞれノードの復元とレイアウトの復元が壊れる

## メディア参照

パスは相対 / 変数形式で記録する（v4 で `assets/refs.json` を廃止）。
絶対パスは `Absolute` として読める。リンク切れはオフライン表示になり、
修復（Relink）は `MEDIA-7`。

**同一性と表示名は別物**（v9、REQ-PROJ-001）。`Document::media_assets` のキーは
`AssetId` で、**再利用されない**。表示名は `MediaAssetEntry::name` にあり、
参照は誰も名前で引かない。だから:

- 素材を削除して同名ファイルを入れ直すと**別の素材**になり、古い参照は
  黙って繋がるのではなくオフラインとして現れる
- 名前は自由に変えられる（改名 UI は `AID-3`）。名前は**一意ではない**ので、
  `Document::media_asset_id_by_name` は 2 件以上一致したら `None` を返す
- プロジェクト間でレイヤーをコピーしても、コピー先に同じ `AssetId` は
  無いので別物を指さない

## テスト

- ラウンドトリップ（保存 → 読込 → 等価）を必ず書く
- マイグレーションは**旧バージョンの実データ相当**を入力にする
- 追加フィールドは「欠落した入力が既定値で読める」ことを検査する
- 任意エントリは「欠落しても開ける」「壊れていても開ける」の両方を検査する
  （`a_project_without_the_opt_in_writes_no_layout_entry` /
  `an_unreadable_embedded_layout_degrades_to_none`）
