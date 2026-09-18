# ノードの Transform セクション実装計画

> **Status**: 未着手 — 2026-09-18

対象: `ravel-core` の `registry`（`NodeTemplate`）、`ravel-nodes` の
`processor_for_node` と `geometry`、`ravel-ui` の `properties`、`ravel-app` の
Viewer オーバーレイと Properties、`assets/locales`。
要件は `REQ-UI-002`（パラメータ編集）、`REQ-UI-011`（Viewer ツールと移動
セマンティクス）、`REQ-MOGRAPH-001`（シェイプ）、`REQ-MOGRAPH-004`
（タイポグラフィ）。

**きっかけは「ジオメトリ系ノードに `geometry.transform` をわざわざ挿すのが
だるい。それこそ Text とか」という観察**（2026-09-18）。

## 問題

### 1. 置き直すためだけにノードを 1 つ挿す

`shape.*` は `center`（`ParamRole::Position`）を自分で持っているので、
Viewer で掴んで動かせる。持っていないノードはそうではない:

| ノード | 自前の位置 | Viewer で掴めるか |
|---|---|---|
| `shape.rect` / `.ellipse` / `.polygon` / `.star` / `.line` | `center`（`line` は `start` / `end`） | ✅ |
| `scatter.grid` / `.circular` / `.scatter` | `center` | ✅ |
| `geometry.sort` | `center` | ✅ |
| `text.layout` | **無い**（`feat/text-layout-position` で `position` を追加中） | 追加後は ✅ |
| `geometry.from_image` | **無い** | ❌ |
| `geometry.merge` で束ねた枝 | **無い** | ❌ |

**位置を持っているノードにも足りないものがある。** `shape.*` と
`scatter.*` は動かせるが、**回す・縮めるには結局 `geometry.transform` が要る**
（`rotation` / `scale` / `pivot` を持たない）。だから本計画は
「位置が無いノードに位置を与える」話に留まらない。

`REQ-UI-011` の移動セマンティクスは、bbox ドラッグで動かせるのを
**「位置を自身のパラメータに持つノード」**、`PathPoints` を持つノード、
**直下流に既存の `geometry.transform` があるノード**に限っている。同じ節が

> ツールによる暗黙のノード自動挿入は行わない（ノードグラフが正という原則を守る）

と明示しているので、「ドラッグしたら transform を挿す」は**要件が禁じている**。
残る道は「ノードが自分で持つ」か「持たせる仕組みを作る」。

### 2. レイヤー殻では足りない

レイヤー 1 枚を動かすなら殻の transform（`position` / `rotation` / `scale` /
`anchor_point`）があり、`ShellManipulator` が Viewer でそれを書く。足りないのは
**1 つのネットワークの中の枝**を置き分けたいとき — テキストを 2 つ並べる、
シェイプとテキストを組む、`scatter` の出力を回す。そこは枝ごとに
`geometry.transform` を挿すしかない。

### 3. 同じ実装が N 個に散る

`shape.*` は 5 ノードがそれぞれ `center` を読んで自分で適用している。
ここに `rotation` / `scale` / `pivot` を足すと、**同じアフィンの適用が
ノードの数だけ増える**。`geometry.transform` の
`GeometryTransformProcessor`（`crates/ravel-nodes/src/geometry.rs:53`）が
既に「scale → rotate → translate、`pivot` と `use_centroid`、点と
インスタンス配置の両方」を持っているのに、それを呼べない形になっている。

## 目標アーキテクチャ

### 宣言 1 行、適用は 1 箇所

```rust
// crates/ravel-core/src/registry/mod.rs
impl NodeTemplate {
    /// This node's geometry output can be moved, turned and scaled by
    /// parameters of its own, applied after the processor runs.
    pub fn with_transform_section(self) -> Self { … }
}
```

宣言すると、テンプレートに `geometry.transform` と**同じ綴りの**パラメータが
足される（`translate` / `rotation` / `scale` / `pivot` / `use_centroid`、
`channel3_parameter`、範囲も同じ）。Properties では `transform` グループに入る。

適用は `processor_for_node`（`crates/ravel-nodes/src/lib.rs:123`）の**1 箇所**。
あの関数は `type_key` の `match` 1 つでプロセッサを返しているので、

```rust
let processor = match node.type_key.as_str() { … };
processor.map(|inner| transform_section::wrap(node, inner))
```

とラップするだけで済む。ラップするかどうかは、コアの
`registry::builtin::has_transform_section(type_key) -> bool`（テンプレートを
定義している場所が単一の正）で決める。

### 合成ノードは作らない

レイヤー殻の合成チェーン（`composition/compile.rs`）は
`NodeRole::Transform` の**合成ノード**を作る形を取っている。同じ手を
ノードごとに使う案は却下する。理由が 3 つある。

1. **ノード id の導出に空きが無い。** 合成 id は
   `comp_id[31:0] << 32 | layer_id[23:0] << 8 | role[7:0]`
   （`compile.rs:54`）で、レイヤー単位だから収まっている。ノードごとの
   合成 transform は 64 bit のノード id を鍵にすることになり、
   **ドキュメント全体で一意**（`REQ-LAYER-009`、`check_unique_node_ids` が
   検証）という不変条件に対して衝突しない導出を新しく発明する必要がある
2. **レイヤーネットワークにはコンパイル段が無い。**
   `compile_composition` の doc が「レイヤーネットワークはグラフに
   展開されない。境界ノードが pull 時に評価する」と明記している。
   ネットワークにコンパイル段を足すのは、この計画より大きい変更
3. **パラメータの方が「グラフが正」と整合する。** 合成ノードは
   ノードエディタで**隠され**、永続化から**除かれる**（`compile.rs` の
   モジュール doc）。つまり「隠れたノード」が増える。対して
   パラメータはユーザーが見ている当のノードの上に出る。
   `shape.rect` の `center` が既にその形であり、**本計画は
   「`shape.*` が 1 ノードずつ持っているものを、宣言で共有する」**という
   位置づけになる

キャッシュと無効化も**変更が要らない**。パラメータはノードのパラメータなので、
既存のパラメータハッシュによる無効化がそのまま効く（合成ノードなら
キャッシュ同一性の側に手を入れることになる）。

### 適用の中身は `geometry.transform` のものを共有する

`GeometryTransformProcessor::process` の本体を、入力と
`ResolvedParams` から `Geometry` を返す関数に切り出し、**ノードも
セクションも同じ関数を呼ぶ**。`ops::stroke_reach` を
`rasterize` と `drawn_bounds` で共有したのと同じ形
（`done/geometry-drawn-bounds-plan.md` の `BBOX-2`）。

- 恒等のときは**入力をそのまま返す**（既存の処理が既に恒等を短絡している）
- 切り出し先は `ravel-nodes`（プロセッサもラッパーも同じクレート）。
  **コアへ移す必要は無い** — コアが持つのは宣言だけ

### 内在的な位置と外在的な変換を混ぜない

`shape.rect` の `center` は「矩形がどこにあるか」で、**内在的**。
`text.layout` の `position`（`feat/text-layout-position`）も「文字原点が
どこか」で内在的。セクションが足すのは**外在的**なアフィン。だから

- **内在的な位置を持つノードは、それを残す。** セクションの `translate` に
  `ParamRole::Position` を宣言しない（1 ノードに Position が 2 つあると、
  マニピュレータは `find` で最初に当たった方を書く —
  `viewer/overlay.rs:2338`。曖昧さを作らない）
- **内在的な位置を持たないノードは、セクションの `translate` が
  `ParamRole::Position` を持つ。** そこで `REQ-UI-011` の移動可能集合が
  「セクションを宣言したノード」に広がる

## 実装単位

### 単位 1（`TFORM-1`）: 宣言と共有適用

- `NodeTemplate::with_transform_section()` をコアに追加。
  パラメータ・範囲・グループを 1 箇所で足す
- `registry::builtin::has_transform_section(type_key) -> bool` を追加
  （宣言したテンプレートの集合。**網羅的な列挙にして、新しいノードを足した
  ときに落ちる形**にできるならそうする）
- `GeometryTransformProcessor::process` の本体を
  `pub fn apply_transform(geometry: &Geometry, params: &ResolvedParams)
  -> anyhow::Result<Geometry>` に切り出し、ノード側はそれを呼ぶだけにする
- `transform_section::wrap(node, inner) -> Arc<dyn NodeProcessor>` を
  `ravel-nodes` に追加。`inner` の出力が `Geometry` のときだけ
  `apply_transform` を通す（`Geometry` でなければ素通し）
- **ラッパーは値を構築時に捕まえない。** `translate` / `rotation` / `scale` /
  `pivot` は `process` に渡される `ResolvedParams` から**毎回**読む。
  理由: `GpuEvalHooks::sync` は、`rebuild_on_node_change()` が `false` の
  プロセッサに対してパラメータ編集で `invalidate_node` だけを呼び、
  **`processor_for_node` を通らない**（`crates/ravel-nodes/src/eval_hooks.rs:246`）。
  構築時に捕まえた値は、そのノード型では**編集後も古いまま残る**
- **`rebuild_on_node_change()` は内側のプロセッサへ委譲する。** ラッパーが
  無条件に `true` を返すと、オプトアウトしている GPU プロセッサが編集の
  ティックごとにシェーダ再コンパイルとパイプライン生成を払う
  （同じコメントがその代償を書いている）
- `processor_for_node` の返り値を 1 箇所でラップする
- **この単位ではどのテンプレートにも宣言を足さない**（機構だけ）

**完了条件**

- `with_transform_section()` を宣言したテンプレートが、
  `geometry.transform` と**同じ綴り・同じ既定値・同じ範囲**の
  パラメータを持つ（`builtin.rs` の既存の arity テストと同じ形で固定する）
- セクションを宣言したノードの出力が、同じ値の `geometry.transform` を
  直下流に挿した結果と**一致する**（数値を書かず関係で固定する）
- セクションが恒等のとき、出力が**入力と同一**（`Arc` の同一性でも
  内容の一致でもよいが、どちらを固定したか報告する）
- 出力が `Geometry` でないノードにセクションを宣言しても素通しする
  （宣言しないので到達しないが、`wrap` が型で落ちないこと）
- `geometry.transform` ノード自体の挙動が 1 ビットも変わらない
  （既存のゴールデンとテストが無改変で緑）
- **`rebuild_on_node_change()` が `false` の内側プロセッサを包んでも、
  セクションのパラメータ編集が次の評価に効く**（構築時に捕まえていない
  ことを、再登録しない経路で固定する）
- ラッパーの `rebuild_on_node_change()` が内側の答えをそのまま返す

### 単位 2（`TFORM-2`）: どのノードに宣言するか

**前提**: `text.layout` の `position`（内在的な位置、`feat/text-layout-position`）
が先に入っていること。入っていないと、`text.layout` のセクションの `translate` に
Position ロールを宣言するかどうかの判断（下記）が逆になる。

- セクションを宣言する:
  - `text.layout`（`position` は内在なので残し、セクションの `translate` は
    Position ロールを持たない）
  - `geometry.from_image`
  - `geometry.merge`
  - `shape.*` と `scatter.*`（どちらも `center` を残し、セクションの
    `translate` は Position ロールを持たない。足りていなかったのは
    `rotation` / `scale` / `pivot`）
- 宣言**しない**: `geometry.transform` 自身（それがセクションそのもの）、
  フレームを出すノード（下記 範囲外）
- 単位 1 の Position ロール規則を、ノードごとに 1 行のコメントで根拠づける

**完了条件**

- `text.layout` に `rotation` を入れると、`text.to_path` を通した結果が
  回った文字列になる（`geometry.transform` を挿した結果と一致）
- `geometry.from_image` の出力をセクションで回すと、`drawn_bounds` が
  **回した矩形の外接矩形**を返す（`done/geometry-drawn-bounds-plan.md` の
  `placed_rect` がそう測る）
- `shape.rect` の `center` の挙動が変わらない。`center` と
  セクションの `translate` は**足し合わさる**（どちらが先かを決めて固定する）
- セクションを持つ全ノードで、パラメータが**既定値のままの文書の出力が
  この変更の前と 1 ビットも変わらない**
- `.ravprj` のフォーマット版を**上げない**（パラメータの追加は加算的で、
  持たない文書は既定で読まれる。`done/layer-content-size-plan.md` の
  「フォーマット版の変更」節と同じ根拠）

### 単位 3（`TFORM-3`）: Viewer と Properties

- Properties: `transform` グループがフォルダとして畳めること
  （既存の `with_param_group` の描画に乗る）
- Viewer: セクションの `translate` に Position ロールがあるノードが
  **bbox ドラッグで動く**。1 ドラッグ = 1 undo、Esc で revert
  （`REQ-UI-011` の既存規約）
- `rotation` / `scale` のハンドルは**この単位では作らない**
  （`ParamRole` に回転・スケールの変種を足す話は別。殻の
  `ShellManipulator` が既にその UI を持っているので、そちらへ寄せるか
  `ParamRole` を広げるかの判断が要る）

**完了条件**

- `geometry.from_image` のノードを選んで bbox をドラッグすると
  `translate` が書かれ、1 undo で戻る
- `shape.rect` を選んだときに書かれるのは**`center` のまま**
  （セクションの `translate` ではない）
- 殻 transform が identity でないレイヤーでは、従来どおり編集させない
  （`REQ-UI-011` の既存規則）

### 単位 4（`TFORM-4`）: ロケールと文書、要件の更新

- `assets/locales/{ja,en}.toml` にセクションのラベル
- `docs/agent-api-reference.md` に `with_transform_section` と
  `apply_transform`
- `docs/dev/add-node.md` のチェックリストに「ジオメトリを出すノードなら
  セクションを宣言するか決める」を 1 行
- **`docs/requirements/REQ-UI.md` の `REQ-UI-011` の移動セマンティクス**に
  「Transform セクションを宣言したノード（その `translate` を書く）」を足す。
  **暗黙のノード自動挿入を禁じる文はそのまま残す** — 本計画はノードを
  挿さないので矛盾しない
- `docs/specifications/` のノード表

**完了条件**

- `mise run docs:check` が通る
- `REQ-UI-011` の移動セマンティクスの一覧が実装と一致する

## 範囲外

- **フレームを出すノードの Transform。** レイヤー殻が `comp.transform`
  （`NodeRole::Transform`）で既に持っている。ピクセルの変換は
  ラスタの範囲（RoD）の話に触るので、そちらが動くまで触らない
  （`done/layer-content-size-plan.md` の範囲外節）
- **3D の `orient` / `scale3`。** `geometry.transform` は既に
  `channel3_parameter` なので Z 成分は通るが、インスタンスの
  `rot` / `scale` は 2D 専用属性のまま（`InstanceTransform` の doc）。
  3D は `3d-scene-plan.md`
- **`geometry.transform` ノードの撤去。** 残す。入力を 1 つ取って
  変換するノードは、セクションと違って**グラフの途中に差し込める**
  （`merge` の後、`scatter` の前）ので役割が別
- **`ParamRole` に回転・スケールの変種を足すこと。** 単位 3 の完了条件が
  translate に限っている理由と同じで、殻のマニピュレータと二重になる
  判断が先に要る
- **`shape.*` の `center` を撤去してセクションに寄せること。**
  内在的な位置なので残す（上記 目標アーキテクチャ）

## 決定（2026-09-18）

1. **合成ノードではなくパラメータ。** 理由 3 つは上記。特に
   「ノードエディタで隠れるノードを増やさない」が効いている
2. **`text.layout` の `position` は先に単独で入れる**
   （`feat/text-layout-position`）。内在的な位置なので本計画と重複せず、
   「Text が Viewer で掴めない」を先に解消できる
3. **セクションのパラメータの綴りは `geometry.transform` と同じにする。**
   別の綴り（`xform_translate` 等）にすると、同じ意味の値が 2 つの名前を
   持つことになる。`shape.*` の `center` と衝突しないことは確認済み
4. **暗黙のノード自動挿入は採らない。** `REQ-UI-011` が明示的に禁じており、
   本計画はノードを挿さないので要件を変えずに済む
