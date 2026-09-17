# レイヤーの内容サイズと `auto` 実装計画

> **Status**: 未着手 — 2026-09-16

対象: `ravel-core` の `registry`（`NodeTemplate`）と `composition`（`base_geometry`）、
`ravel-nodes` の `shape`、`ravel-ui` の `properties::node`、`ravel-app` の
Properties と Viewer オーバーレイ、`assets/layer-templates`。
要件は `REQ-UI-002`（パラメータ編集）と `REQ-UI-013`（Viewer の bbox）、
`REQ-LAYER-008`（レイヤーテンプレート）。

**きっかけは「Solid レイヤーの bbox がコンプ解像度分ある」という観察。**
bbox の計算自体は既に内容ベースで、解像度分になっているのは**内容がコンプ解像度の
四角形だから**だった。直す対象は bbox ではなく、**レイヤーの内容にサイズという
概念が無いこと**。

## 問題

### 1. Solid レイヤーに自分のサイズが無い

`assets/layer-templates/solid.ron` は `net.in` の `base_geometry` 出力を
`rasterize` の `geometry` へ繋ぐ。`base_geometry` は In ノードの固定ポートで、
評価器が**無条件にコンプ解像度の閉じたパス**を答える:

```rust
// crates/ravel-nodes/src/net.rs:137
fn base_quad(resolution: (u32, u32)) -> Geometry {
    let (w, h) = (resolution.0 as f32, resolution.1 as f32);
    Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(w, 0.0), Vec2(w, h), Vec2(0.0, h)])
}
```

（`net.rs:49` が `PORT_BASE_GEOMETRY => base_quad(ctx.comp_resolution)`。）

- **200×200 の Solid が作れない。** After Effects は Solid 生成時に幅・高さを訊く
- Viewer の bbox（`layer_comp_rect`、`crates/ravel-app/src/panels/viewer.rs:5479`）は
  終端ジオメトリノードの bounds を union するので、Solid では `net.in` の
  base quad ＝コンプ全面が答えになる。**bbox は正しく内容を測っていて、
  内容がコンプ全面**という状態
- ハンドルでスケールしても書かれるのは**殻の `scale`** で、内容のサイズではない。
  「200×200 の板」と「コンプ全面を 0.1 倍した板」が区別できない

レイヤー種別ごとの実態:

| 種別 | ネットワーク | 終端ジオメトリノード | bbox |
|---|---|---|---|
| solid | `net.in.base_geometry` → rasterize | `net.in` | **コンプ解像度** |
| shape | `shape.rect` → rasterize | `shape.rect` | 内容ベース（正しい） |
| media | `media` → `net.out` | 無し | **出ない** |
| audio / null | — | 無し | 出ない |

### 2. bbox が内容を取りこぼす経路が 3 つある

コンプ解像度とは別に、**内容を測り損なっている**箇所がある。

| 箇所 | 取りこぼすもの | 症状 |
|---|---|---|
| `geometry_bounds`（`crates/ravel-app/src/panels/viewer/geometry.rs:94`）が Point / Instance の**位置だけ**を見る | **画像インスタンスの矩形** | `geometry.from_image` は「原点に 1 インスタンス、画像は source の rect」という形（`crates/ravel-nodes/src/geometry.rs` の `from_image_outputs_one_instance_stamping_the_image` が明示）。位置は 1 点なので **bbox が 0×0** になり、画像ジオメトリのレイヤーはマニピュレータが出ない |
| コアの `Geometry::positions_bounds`（`crates/ravel-core/src/geometry/container.rs:765`）が `Domain::Point` だけを見る | **インスタンスの位置** | インスタンスしか持たないジオメトリは `bounds()` が 0×0。**コアと Viewer で bbox の定義が食い違っている**（Viewer は自前の `geometry_bounds` で Instance も見る） |
| どちらも線幅を見ない | **ストローク幅** | `rasterize` は `stroke_margin`（`rasterize/mod.rs:143`）で「線が点からどこまで届くか」を持っているのに、bbox は点の範囲で止まる。太い線のシェイプは bbox が内容より小さい |

**`MED-APP-21`（Viewer の bbox が `type_key` の固定 match で再構成される、解決済み）
と同じ種類の穴**で、「bbox は何を測るのか」の答えが 2 箇所に分かれているのが根。

**この 3 件は本計画の単位ではない。** `auto` と独立していて単独でも価値があるので、
`MED-APP-45` / `MED-CORE-11` / `LOW-APP-33` として起票し別に回す。ここに残して
あるのは、単位 3 の完了条件「Solid の bbox がその矩形になる」が**測る側の穴とは
別の話**だと読めるようにするため。

### 3. 「自動で決まる」を表す形が無い

「幅は内容（または解像度）から決まる」を言う手段が `ParameterValue` に無い。

| 要るもの | 今あるもの | 足りないもの |
|---|---|---|
| 数値パラメータの「未設定 / 自動」 | 無い。番兵値しかない | `layer.ref` の `layer = -1` がまさにその番兵で、**`CPO-5` で撤去中**（`contextual-parameter-options-plan.md`）。同じ轍を掘らない |
| 「別パラメータの値で行の見せ方が決まる」宣言 | `ColorParam::When` / `with_color_param_when`（`crates/ravel-core/src/registry/mod.rs:301`） | それは「4 成分を色として描くか」専用。**一般化されていない** |
| 触れないパラメータ行 | 接続されたポートに駆動された行が `PropertyField::ReadOnly` の `"12.000 ← Constant"` になる（`crates/ravel-ui/src/properties/node.rs:233-241`） | **駆動の理由が「エッジ」に限られている** |

3 番目が本計画の土台になる。UX 不変条件 6 は「動かない制御は無効化し、無効に見せる」
なので、`auto` のときに数値が編集できる顔で出ていてはいけない。

## 目標アーキテクチャ

### 新しいノードを作らない — `shape.rect` が既にサイズ付き quad

`shape.rect`（`crates/ravel-core/src/registry/builtin.rs:1643`）は既に
`center`（`Channel2`、`ParamRole::Position`）+ `width` / `height`（`Float`、範囲付き）
を持っている。**足りないのは `auto` だけ。**

したがって新ノードは作らず、`shape.rect` に解像度モードを足し、`solid.ron` を
`base_geometry` 経由から `shape.rect` へ差し替える。Viewer の
`ParamRole::Position` ハンドルもそのまま効く。

### `auto` の意味は「そのノードが導出できる範囲」

```text
sizing: String   // param_options ["auto", "fixed"] → Properties の dropdown
```

- **`auto`** — そのノードが導出できる範囲。入力を持たない `shape.rect` では
  **コンプ解像度**（`center = (w/2, h/2)`, `width = w`, `height = h` で、
  今の `base_quad` と 1 ピクセルも変わらない矩形になる）
- **`fixed`** — `center` / `width` / `height` の値をそのまま使う

**`expand` や `comp` という名前にしない。** 将来 RoD（下記「範囲外」）が入ると、
入力を持つノードでは同じ `auto` が「入力の内容範囲」を意味する。`comp` と
名付けるとそこで名前が嘘になる。

導出は **1 関数に閉じる**。`auto` の解決が 2 箇所に分かれると、Viewer の bbox と
ラスタが食い違う — `MED-APP-21` と同じ壊れ方をする。

### `auto` のときは 3 行が read-only になる

`center` / `width` / `height` は `auto` のとき**編集不能**。行の形は
既存の「エッジに駆動された行」をそのまま使う:

```text
width   1920 ← auto
```

`PropertyField::ReadOnly` で、**新しい variant を作らない**。駆動の理由を
「エッジ」から「エッジ または 宣言」へ広げるだけ。

**`center` も `auto` に含める。** 含めない案（`auto` は `width`/`height` だけ）は
今の `base_quad` を再現できない — base quad は (0,0)–(w,h) なので中心が
`(w/2, h/2)` である必要があり、`center` をユーザー値のままにすると既存 Solid が
動いてしまう。

### 宣言は `ColorParam::When` の一般化として持つ

「このパラメータは別パラメータの値によって解決される」を**レジストリの宣言**に
する。`type_key` の固定 match で分岐しない（`MED-APP-21` で一度払った代償）。

`ColorParam::When { key, value }` が既に同型の判定
（`holds_for(node)`、`registry/mod.rs:127-137`）を持っているので、**その判定を
共有する形**に寄せる。2 本目の「別パラメータを見る」機構を作らない。

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| EXT-1 | `shape.rect` の `sizing` と `auto` の解決（コアと nodes、UI 無し） | — |
| EXT-2 | 宣言で駆動された行を read-only にする（`ColorParam::When` の一般化） | EXT-1 |
| EXT-3 | `solid.ron` を `shape.rect` へ差し替え | EXT-1 |
| EXT-4 | ロケール / 文書 | EXT-1〜3 |

**フォーマット版は上げない。** 理由は「範囲外」の最後に書く。

### 単位 1: `shape.rect` の `sizing` と `auto` の解決

- `shape.rect` に `sizing: String`（**既定 `"fixed"`**）を足し、
  `with_param_options("sizing", ["auto", "fixed"])` を付ける
- `auto` の矩形を返す関数を 1 つ置き、`shape.rect` のプロセッサがそれを使う。
  入力を持たないので導出は `ctx.comp_resolution` から
- **既定は `"fixed"`。** 既存プロジェクトの `shape.rect` は `sizing`
  パラメータを持たないのでテンプレート既定が読まれる。`auto` を既定にすると
  **既存のシェイプ（100×100 の板）が全部コンプ全面に化ける**。
  `auto` が要るのは Solid だけなので、**`solid.ron` が `"auto"` を明示して持つ**
  （テンプレートはノードのパラメータ値を書けるので、これで足りる）。
  結果として既存文書を 1 バイトも触らず、フォーマット版も消費しない

**完了条件**

- `sizing = "fixed"` のとき、`center` / `width` / `height` の意味が今と 1 つも変わらない
- `sizing = "auto"` のとき、出力ジオメトリが `base_quad(ctx.comp_resolution)` と
  **頂点単位で一致する**（既存の `base_quad` を期待値に使うテスト）
- コンプ解像度を変えると `auto` の矩形が追従する
- `sizing` パラメータを持たない（= 既存プロジェクトの）`shape.rect` が
  **今と同じ矩形**を出す

### 単位 2: 宣言で駆動された行を read-only にする

- `PropertyField::ReadOnly` で `"1920 ← auto"` の形にする。
  **新しい `PropertyField` variant を作らない**
- 駆動の理由を「エッジ」から広げる。`DrivenParam`
  （`crates/ravel-ui/src/properties/mod.rs:235`）は
  `{ key, source, value }` なので、`source` に `"auto"` を入れれば
  既存の描画経路がそのまま使える
- **エッジによる駆動が優先**。`width` にエッジが繋がっていて、かつ
  `sizing = "auto"` のときは、エッジの理由を出す（そちらの方が具体的）
- 宣言は EXT-1 のレジストリ側に置き、`type_key` の match にしない

**完了条件**

- `sizing = "auto"` のとき 3 行が read-only になり、**解決済みの値と `auto` を出す**
- `sizing = "fixed"` に戻すと 3 行が編集可能に戻る
  （`field_shape_key` が変わるのでウィジェットが作り直される）
- エッジで駆動された行は `auto` のときもエッジの理由を出す
- **行が「押せるのに何も起きない」状態にならない**（不変条件 6）

### 単位 3: `solid.ron` を `shape.rect` へ差し替え

- `solid.ron` のノードを `net.in.base_geometry` → `rasterize` から
  `shape.rect`（`sizing: "auto"`）→ `rasterize` に変える
- **既存プロジェクトのグラフは書き換えない。** `base_geometry` は In ノードの
  宣言された固定ポート（`REQ-LAYER-002`/`003`）で他の用途もあり、
  任意の便宜のためにユーザー文書へグラフ手術をするのは割に合わない。
  旧 Solid は base quad のまま動き続ける（`auto` の値と同一なので見た目は不変）。
  サイズが欲しいユーザーはノードを差し替える

**完了条件**

- 新規 Solid レイヤーの見た目が今と変わらない
- 新規 Solid の `sizing` を `fixed` にして幅・高さを入れると、
  **Viewer の bbox がその矩形になる**
- 旧 `.ravprj` の Solid が読めて、見た目が変わらない

## 範囲外

- **ラスタの範囲（RoD）。** これは別の計画書。現況は、`FrameBuffer` が
  `width / height / format / data` だけで**原点を持たず**
  （`crates/ravel-core/src/types.rs`）、`rasterize`（`rasterize/mod.rs:374`）/
  `net.out` のフォールバック（`net.rs:149`）/ `media`（`media.rs:158`）/
  `merge`（`comp/merge.rs:187,250`）が全部 `ctx.resolution` で確保し、`merge` が
  範囲外を透明として読む（左上原点合わせ）。つまり本計画で 200×200 の Solid を
  作れるようにしても、**ラスタは依然コンプ解像度で周りが透明になるだけ**で、
  コンプ枠外へはみ出した内容は保持されない。
  `REQ-PLUGIN.md`（OFX の画像モデルは矩形 RGBA + RoD / ROI）と
  `ofx-host-plan.md` の `OFX-4` 完了条件「RoD / ROI が Ravel の bbox と矛盾しない」が
  いずれ強制するが、**前提が動くうちに計画書を書かない**（`REQ-PLUGIN` 自身が
  同じ理由で OFX の計画着手を `GPUBK-8` の後に置いている）。

  **本計画が RoD を塞がないために守ること**:
  1. サイズを**ノードのパラメータ**として持つ。`Layer` 殻に置かない
     — RoD は入力から出力へ伝播する性質なので、殻フィールドは必ず 2 つ目の正になる
  2. `auto` を「コンプ解像度」と**定義しない**。「そのノードが導出する値」と定義し、
     `shape.rect` の実装が「導出＝コンプ解像度」と答える。RoD が入ったら
     入力を読む分岐を 1 関数に足すだけで済む
  3. `FrameBuffer` に原点を足さない
- **`width` / `height` のアニメート。** 今は `Float` で、`Channel` にすると
  保存済みの値の読み替えが必要になる（`VEC-5` / `PARAM-1` と同じ種類の移行）。
  `auto` と独立した話なので分ける
- **Media レイヤーの bbox。** `media` → `net.out` にジオメトリノードが無いので
  マニピュレータが出ない。素材の実寸から bbox を出すには
  「フレームを出すノードの範囲」という概念が要るので RoD 側の話
- **bbox ハンドルで内容のサイズを編集すること。** 今ハンドルが書くのは殻の
  `scale` で、それは変えない（`REQ-UI.md` の移動セマンティクスが
  「位置を自身のパラメータに持つノード」に限っている規則と同じ線）
- **フォーマット版の変更。** `sizing` は**新しいパラメータの追加**で、
  既存の値の読み替えではない。パラメータは自由な key/value の並びなので、
  持たない文書はテンプレート既定が読まれる（`param_fold.rs` が
  「v4 文書の `center_x` はそのまま deserialize されて読まれなくなるだけ」と
  説明しているのと同じ性質）。ただし**既定が `auto` だと既存シェイプが化ける**ので、
  「パラメータの不在」と「`auto` の明示」を区別する必要がある — 下記「判断」

## 決定（2026-09-16）

1. **`sizing` の既定は `"fixed"`、`solid.ron` が `"auto"` を明示する。**
   既存文書を触らず、フォーマット版も消費しない。既定を `"auto"` にして
   `.ravprj` v13 で既存 `shape.rect` に `"fixed"` を書き込む案と、Solid 専用
   ノードを別に作る案は却下した — 前者はフォーマット版を消費し、後者は
   「サイズ付き quad」が 2 つになる
2. **キー名は `sizing`**、値は `auto` / `fixed`。`extent` を使わないのは、
   **ラスタの範囲（RoD）の語として空けておく**ため — そちらが入ると
   「そのノードが出す画像の範囲」を指す語が要る。`sizing` は「どう寸法が
   決まるか」でモード名として読め、`extent` は範囲そのものを指す名詞なので、
   モードの値として `extent = "auto"` と書くと意味がずれる。
   なお既存のモード列挙は `mode` / `<thing>_mode` という綴りが多い
   （`field.time` の `mode`、`scatter` の `source_mode`、`text` の `writing_mode`）
   ので、`size_mode` でも規約には合う。1 語で済む方を採った
3. **bbox の取りこぼし 3 件は本計画から外し、issue として独立で回す**
   （`MED-APP-45` / `MED-CORE-11` / `LOW-APP-33`）
