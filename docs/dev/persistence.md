# 永続化を変更する

> 索引: [`README.md`](README.md)

`.ravprj` の構造とフォーマットバージョンの扱い。データモデルの定義は
[`../specifications/data-model.md`](../specifications/data-model.md)。

## `.ravprj` の中身

| エントリ | 内容 |
|---|---|
| `manifest.json` | `format_version` とプロジェクト情報。**マイグレーション連鎖の起点** |
| `document/main.ron` | Composition・レイヤー・ネットワーク（Subnet 入れ子含む）・キーフレーム・`media_assets`。決定的 RON |
| `settings.toml` | プロジェクト設定 |
| `ui_state.json` | UI 状態（アクティブコンポジション）。**任意エントリ**で、欠落時は `root_comp` にフォールバック |
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
- **アトミック書き込みは共有する。** `crates/ravel-app/src/project/atomic_write.rs`
  が temp ファイル + sync + 名前入れ替えの 1 実装で、`.ravprj` の書き込みも
  これを使う（Windows の置換プリミティブを 2 箇所に持たないため）
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

## 判断: バージョンを上げるか、上げないか

```text
既存フィールドの意味・型・単位を変える     → バージョンを上げてマイグレーションを書く
新しいフィールドを足すだけ                → 上げない。#[serde(default)] で読む
新しい任意エントリを足すだけ              → 上げない
```

`ParameterValue` に variant を足すのは別枠。**必ず末尾に足し**、
`JOURNAL_FORMAT_VERSION`（`ravel_core::undo::journal`）を上げる。bincode は
variant を位置で索引するので途中挿入は旧 journal を壊す。`.ravprj` の
`format_version` は、既存パラメータの表現を変えるとき（v6 のカーブ）だけ
上げる — variant を足すだけなら上げない。

**追加フィールドでバージョンを上げない**のは前例がある: `Layer.audio` は
format v4 のまま `#[serde(default)]` の追加フィールドとして入り、
マイグレーションも作っていない。`ui_state.json` も format_version 3 のまま
追加された。

この判断は `docs/implementation/roadmap.md` の基準 1（移行コストが時間で
増える単位を先に）にも影響する。**フィールド追加だけなら後回しのコストは
上がらない**ので、基準 1 を根拠に前倒しする理由にはならない。

## マイグレーションを書く

`crates/ravel-app/src/project/migration.rs`。`migrate_to_current` が
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

前例が 2 つある。

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
  並べ替え、重複入力を畳む。**壊れた入力でロードを失敗させない**のが方針
- **消せない差は文書に書く。** カーブ変換に残る差は 2 つ（非有限値の点を落とす、
  重複入力の段差を後の点に畳む）。どちらも Ravel が書かない入力に限られるが、
  コードのモジュールコメントと実装計画の両方に明記する

## ID の扱い

- ロード時に `NodeId` / `EdgeId` / `CompId` / `LayerId` のカウンタを
  ドキュメント最大 ID より先へ進める（REQ-LAYER-009）。新しい ID 種を足したら
  ここも足す
- `type_key` とパネルの `panel_id()` は**永続化に載る文字列**。後から変えると
  それぞれノードの復元とレイアウトの復元が壊れる

## メディア参照

パスは相対 / 変数形式で記録する（v4 で `assets/refs.json` を廃止）。
絶対パスは `Absolute` として読める。リンク切れはオフライン表示になり、
修復（Relink）は `MEDIA-7`。

## テスト

- ラウンドトリップ（保存 → 読込 → 等価）を必ず書く
- マイグレーションは**旧バージョンの実データ相当**を入力にする
- 追加フィールドは「欠落した入力が既定値で読める」ことを検査する
- 任意エントリは「欠落しても開ける」「壊れていても開ける」の両方を検査する
  （`a_project_without_the_opt_in_writes_no_layout_entry` /
  `an_unreadable_embedded_layout_degrades_to_none`）
