# 画像インスタンス実装計画（FrameBuffer を並べる）

> **Status**: Done — 2026-08-14。`IMG-1`〜`IMG-6` がすべて入った。決定事項
> （下記 9 項目）は**ユーザー合意済み**（2026-08-06）。着手は `IMG-1` だけ
> 先行し、残りはロードマップのフェーズ C4 の後に回した（決定 9）。
>
> 実装後に確定した点:
>
> - **テクスチャ束縛は案 (c)（ソースが変わる境目で描画分割）で入った。**
>   `QuadDraw` が `QuadRun`（テクスチャ + インスタンス範囲）の列を取り、
>   1 つのレンダーパスに run ごとの draw を積む。順序は分割の構造として
>   保たれる。パスと点だけの run は読まない 1px のプレースホルダを束ねる
> - **`Geometry::instance_sources()`（geometry 専用の複数形ビュー）は削除した。**
>   下記「未解決の問い」を参照

対象: FrameBuffer をジオメトリのインスタンスソースとして扱い、**既存の
instance 機構でそのまま複製できる**ようにする。AE のリピーター / C4D の
クローナーに相当する操作を、画像に対して開く。

関連要件: REQ-3D-001、REQ-MOGRAPH-001、REQ-CORE-010、REQ-CORE-009。

## 問題

要件の出所は「FrameBuffer を Repeat したい」。レイヤーの出力（動画・テキスト・
シェイプの合成結果）を格子や円周に並べたい、という操作である。**現状これが
できる経路は 1 つも無い。**

### 現状表

> **これは 2026-08-06 の起票時点**。`IMG-1` が `SceneContent::Image` を畳んだ
> 後は、下の 2 行（`SceneContent::Image` / `SceneImage`）はもう成立しない —
> **画像を Scene に置く手段自体が無くなり**、`geometry.from_image`（`IMG-3`）が
> 入るまでその状態が続く。問題の出発点として残してある。

| やりたいこと | 現状 | 理由 |
|---|---|---|
| ジオメトリを格子に並べて描く | できる | `scatter.grid` → `rasterize`。instance ドメインの `P` / `rot` / `scale` / `Cd` / `alpha` が効く |
| コピーごとに違うジオメトリを使う | できる | `instance_sources` が複数持てて、instance の `source_index`（I32）が選ぶ（`rasterize/mod.rs:740` の `select_instance_source`） |
| FrameBuffer を格子に並べて描く | **できない** | 画像を置ける入れ物は `SceneContent::Image` だけで、それを描くレンダラ（`scene.render`）が未実装 |
| FrameBuffer を Scene に置く | 型としては置ける | `scene.add` が `SceneImage` を作る（`crates/ravel-nodes/src/scene.rs:55`）。ただし **`Scene` を消費するノードが 1 つも無い**ので、置いた結果は誰にも届かない |
| コピーごとに違う画像 | できない | 上記のとおり並べる経路が無い |

`rasterize` がメッシュを拒否する理由と混同しないこと。**`rasterize` は
analytic かつ planar なラスタライザ**で、フラグメントごとにパスセグメントへの
距離を評価する。頂点バッファも深度アタッチメントも持たない
（`ensure_planar_paths` の doc コメント、`crates/ravel-nodes/src/rasterize/mod.rs:390`）。
テクスチャ付き矩形はこの方式で描けるが、変形（コーナーピン、ワープ）は描けない。

### `SceneContent::Image` の消費者はゼロである（決定 4 の根拠）

> **これは 2026-08-06 の調査時点の記録**で、`IMG-1` が実際に畳んだ後の姿では
> ない。下の行番号はもう存在しない — 決定の根拠として残してある。

`rg -n "SceneContent|FlatContent|SceneImage" --type rust crates/` の全ヒットは
2 ファイルに閉じていた。

| 場所 | 種別 |
|---|---|
| `crates/ravel-core/src/scene/mod.rs` の 45 / 128–222 / 250 / 276–283 | 定義・コンストラクタ |
| 同 391–399（`collect_flat`）、408–410（`holds_gpu_resident`）、453–455（`byte_size`） | **自分自身の走査**（`match` の網羅） |
| 同 477 以降 | `#[cfg(test)]`（テストモジュールは 468 行目から） |
| `crates/ravel-nodes/src/scene.rs:43–66`（`scene_content`）、`:55` | **生成側**（`scene.add`） |
| 同 211 行目以降 | `#[cfg(test)]` |

**画像バリアントの値を読んで何かを描く / 出力する / 永続化するコードは存在しない。**
`rg -n "DataTypeId::SCENE" --type rust crates/` も、`Scene` を入力に取るノードが
`scene.add` / `scene.merge` / `scene.camera` の 3 つ（いずれも Scene を返す
アセンブリノード）しかないことを示す。`scene.render`（`3D-4`）は未着手。

つまり今この変種を消しても、**壊れる利用者はテストしかいない**。

## 決定事項

以下は 2026-08-06 のユーザー合意。**却下した案を残してあるのは、後から
「なぜこうしたか」を辿れるようにするため**であって、再検討の余地の表明ではない。

### 決定 1: 複製の意味は「焼き付け」

**ネットワークの評価は 1 回だけ。得られた FrameBuffer を N 箇所にスタンプする。**

- コピーごとに変えられるのは位置・回転・スケール・色・α だけ
  （= 既存の instance ドメインの属性）
- **コピーごとにネットワークを再評価しない。**
  `per-instance-modulation-plan.md` と `evaluation-scope-plan.md` が両方とも
  「per-instance subgraph re-evaluation は導入しない」と明記しており、
  その線を越えない
- 却下した案: コピーごとに別の時刻 / 別パラメータで再評価（C4D の time offset
  相当）。評価回数がコピー数分になり、キャッシュが効かない

### 決定 2: ジオメトリ側で解く

**画像を `Geometry` に載せ、既存の instance 機構をそのまま使う。**

- `scatter.*` / `field.apply` / `geometry.repeat`（`OPS-7`）/
  `geometry.blast`（`OPS-1`）/ `geometry.iterate`（`SCOPE-3`）が、
  相手が画像だと知らないまま動く
- 代償: `rasterize` にテクスチャ経路を生やす
- 却下した案:
  - **合成側で閉じる**（`Geometry`（配置）+ `FrameBuffer` → `FrameBuffer` の
    新ノード）。安いが画像がジオメトリ演算の対象にならず、世界が 2 つに割れる
  - **3D の Scene 経由**。`scene.render`（`3D-4`）が未実装なので、
    2D のリピーターのために三角形レンダラを待つことになる

### 決定 3: 画像は instance source に載せる

**`instance_sources: Vec<Arc<Geometry>>`（`crates/ravel-core/src/geometry/container.rs:198`）
を `Vec<InstanceSource>` へ一般化する。**

```rust
enum InstanceSource {
    Geometry(Arc<Geometry>),
    Image(/* 決定 6 の持ち方 */),
}
```

- 形の前例は既存の `SceneContent`（`crates/ravel-core/src/scene/mod.rs:217`）。
  発明ではない
- **point / primitive ドメインは触らない。** `Primitive` に画像バリアントを
  足さない、UV 標準属性を足さない。それらは変形（コーナーピン、ワープ）が
  要るときの話で、今その負債を先払いしない
- **`source_index`（`names::SOURCE_INDEX`、I32、instance ドメイン）が既にあるので
  「コピーごとに違う画像」は新機構ゼロで出る** — N 枚を `instance_sources` に
  入れて `source_index = index` にするだけ。`scatter.*` の
  `attach_instance_sources`（`crates/ravel-nodes/src/scatter/mod.rs:95`）が
  複数ソース時にこの列を既に書いている。コンタクトシートがそのまま作れる

#### `geometry.from_image` の出力形

**画像を instance source に持つインスタンス 1 個**を出す。

- 単体で `rasterize` すれば原点に 1 枚描かれる
- `scatter.grid` に食わせると入れ子（深さ 2）で N 枚になる。
  `MAX_INSTANCE_DEPTH = 4`（`rasterize/mod.rs:39`）の範囲内
- ゼロインスタンスにすると単体で何も描かれず、ノードとして壊れる

### 決定 4: `SceneContent::Image` は退場させる

**`SceneContent` を `Geometry` / `Scene` の 2 バリアントにする。**
画像も `Geometry` を通る。`scene.add` は Geometry と Scene だけ受け、
FrameBuffer を置きたいユーザーは `geometry.from_image` を 1 つ挟む。

- **今なら安い**。上記「消費者はゼロである」のとおり `scene.render`（`3D-4`）が
  未着手で `FlatContent::Image` の読み手がいない。`roadmap.md` の基準 3
  （挙動不変のリファクタは分岐が増える前に）そのもの。`3D-4` / `3D-5` / `3D-7`
  が両方の上に積んでから消すと跳ね上がる
- 3D 配置は instance の `P`（Vec3 可、`3D-1a` 済）/ `orient`（クォータニオン）/
  `scale3`（いずれも `3D-2` 済）で表現できるので、**REQ-3D-001 の
  「FrameBuffer を置いたらテクスチャ付き矩形」は instance-image で満たせる**
- **REQ-3D-001 の本文修正が要る**（「オブジェクトはジオメトリまたは
  FrameBuffer」→「ジオメトリ経由で矩形になる」）。既知の制約に書かれている
  「FrameBuffer オブジェクトは複製できない」も本計画が解消する側になる。
  併せて `docs/requirements/overview.md` の整合を見る
- `Scene` は `Serialize` を実装しておらず `Geometry` も同様なので、
  **`.ravprj` の移行は発生しない**
- 却下した案: 残して二重にする / `3D-4` 着手時に判断を先送りする

### 決定 5: 解像度は「ソース解像度 = コンポ単位」

**画像のピクセル寸法がそのままコンポ単位。** アスペクト比が構造上保たれる
（既存 `SceneImage::rect()` と同じ規約、REQ-3D-001 の考え方）。

- scale=1 のコピーはソースのピクセル寸法を占める。矩形は原点中心
- **拡大するとボケる。これを仕様として文書に明記する。黙ってボカさない**
- 却下した案:
  - `geometry.from_image` に `resolution_scale` を持たせて上流を高解像度で評価し直す
    （評価スコープを書き換えるので `SCOPE-*` の領分に触る。VRAM も係数の 2 乗）
  - コピーごとに必要解像度で再評価（決定 1 と矛盾する）

### 決定 6: 画像は到着した表現のまま持つ

**`Arc<dyn NodeData>` をそのまま抱える。** CPU `FrameBuffer` でも
`GpuFrameBuffer` でも受ける。

- 前例: `SceneImage.frame: Arc<dyn NodeData>` と、その doc の
  「GPU 常駐フレームがリードバック無しで通る」
  （`crates/ravel-core/src/scene/mod.rs:131-144`）。`ravel-core` は
  `ravel-gpu` を知らないので、この持ち方以外に選択肢が無い
- **リードバックを発生させない。** `HIGH-04` / `HIGH-08` / `HIGH-09` と
  `gpu-compositing-plan.md` が消したボトルネックを復活させないことが理由
- 代償: **画像を抱えた Geometry は VRAM を握る**（`GpuFrameBuffer` は
  `Arc<PooledHandle>` で、最後のクローンが落ちるまでプールへ返らない —
  `crates/ravel-gpu/src/frame.rs:32-63`）。`cache-plan.md` の `CACHE-3`
  （VRAM 予算）の会計に「画像付きジオメトリ」を入れる必要がある。
  下記「未解決の依存」を参照
- 却下した案: CPU に正規化して持つ（レイヤー出力を repeat する典型例で
  毎フレームリードバックが入る）/ id 参照 + 別管理の画像レジストリ
  （不変 DAG の外に状態が出る）

### 決定 7: 重なりと合成は既存規則のまま

**新しい規則を作らない。** `raster_instances`
（`crates/ravel-nodes/src/rasterize/mod.rs:690`）は既にインスタンスを
index 順に `pixels` へ描き込むペインタ方式で、tint は `Cd` × `alpha` の乗算
（`tinted()`）。

- コピー i がコピー i-1 の上に乗る
- 画像インスタンスは texel に tint を掛ける
- **コピーごとのブレンドモード（add / multiply / screen）は今回やらない。**
  instance ドメインに blend 属性が無く、ブレンドはレイヤー階層
  （`comp.merge.*`）が持つ概念。足すなら分離可能な増分として後で

### 決定 8: rasterize のテクスチャ経路は analytic に留める

`ensure_planar_paths`（`rasterize/mod.rs:390`）がメッシュを弾いているのは
縄張り争いではなく、**rasterize が analytic かつ planar** だから。
黙って空フレームを出さないための大声のガードである。

- **アフィン配置のテクスチャ付き矩形は analytic に描ける** — 配置行列を
  逆変換してフラグメントごとにテクスチャをサンプルするだけ。
  `DrawItem`（`rasterize/mod.rs:185`、`bounds / color / data0 / data1`）に乗る
- **`ensure_planar_paths` のメッシュ拒否は残す。** 変形（コーナーピン、ワープ、
  ページカール、3D 射影）は頂点と三角形が要るので `scene.render` の管轄。
  そのときは `to_mesh`（UV 付きメッシュ）を別途足し、rasterize は既存の
  ガードで**正しく大声で拒否**する
- **緩めるのは instance source の再帰だけ**である。`ensure_planar_paths` は
  `geo.require_paths("rasterize")` を自分自身に掛けたうえで
  `geo.instance_sources()` を再帰しているので、`InstanceSource::Image` は
  再帰対象から外す（画像はプリミティブを持たないので検査するものが無い）。
  `Geometry::require_paths` そのものは変更しない

### 決定 9: 着手順 — 計画書を先に書き、実装は C4 の後

- **今回やる**: 本計画書を書く。加えて **`IMG-1`（`SceneContent::Image` の退場）
  だけは早めに引き取る**（決定 4 の理由、`roadmap.md` の基準 3 で安いうちに）
- **次**: フェーズ C4（書き出し / 式言語）を既定の順で進める。ロードマップの
  基準 0（「今何もファイルにできないので、ここが開くまで他の投資が
  回収されない」）を崩さない
- **その後**: `IMG-2` 以降（`geometry.from_image` + `rasterize` のテクスチャ経路 +
  GPU 展開への反映）
- 却下した案: C4 に割り込ませる / 最小の 2D 経路だけフェーズ C 末尾に入れる
  （後者は決定 4 と矛盾する — 統一の半分だけ入った状態が残る）

## 目標構成

```text
レイヤー出力 / media / rasterize
        │  FrameBuffer（CPU または GPU 常駐）
        ▼
   geometry.from_image                         ← IMG-3
        │  Geometry:
        │    instances = 1 個（P = (0,0)、index = 0）
        │    instance_sources = [ InstanceSource::Image(frame, w, h) ]
        │    points / primitives = 空
        ▼
   scatter.grid / scatter.circular / geometry.repeat / field.apply / …
        │  既存の instance 機構。画像だと知らないまま動く
        │  Geometry:
        │    instances = N 個（P / rot / scale / Cd / alpha / source_index）
        │    instance_sources = [ Geometry(geometry.from_image の出力) ]   ← 深さ 2
        ▼
   ┌────────────────────┬──────────────────────┐
   │ rasterize（CPU）    │ rasterize（GPU）      │  ← IMG-4 / IMG-5
   │ raster_instances    │ flatten_geometry      │
   │  → 逆変換して        │  → DrawItem(kind=2)   │
   │    バイリニア標本    │    + テクスチャ束縛    │
   └────────────────────┴──────────────────────┘
        │  FrameBuffer
        ▼
   殻の合成チェーン / Viewer
```

`scene.render`（`3D-4`）が入ったときは、同じ `Geometry` が
`SceneContent::Geometry` として Scene に載る。**画像を置く経路が Scene 側に
二重に存在しない**のが決定 4 の狙いである。

```text
（決定 4 の後）
FrameBuffer ──▶ geometry.from_image ──▶ Geometry ──┬──▶ rasterize      ──▶ FrameBuffer
                                           └──▶ scene.add ──▶ Scene ──▶ scene.render
```

## 実装単位

単位 ID の接頭辞は `IMG-`。`backlog.md` の既存接頭辞（`OPS-` / `GPU-` /
`CACHE-` / `3D-` ほか）と衝突しない。

### `IMG-1`: `SceneContent::Image` の退場

**この単位だけ先行して着手する**（決定 9）。挙動不変のリファクタで、
消費者がいないうちに畳む。

- `SceneContent` を `Geometry` / `Scene` の 2 バリアントにする。
  `SceneImage` 型と `SceneObject::image` を削除する。
  `SceneError::NotAFrameBuffer` / `EmptyImage` も併せて退場する
  （`SceneImage::new` 以外に投げ手がいない）。
- `FlatContent` から `Image` を外す。**1 バリアントの enum として残すか
  `Arc<Geometry>` に潰すかは実装時に決める**（下記「未解決の問い」）。
- `scene.add` の `object` ポートから `DataTypeId::FRAME_BUFFER` を外し、
  `scene_content()` の FrameBuffer 分岐を削除する。FrameBuffer を繋いだ
  ユーザーには「`geometry.from_image` を挟め」と読める明示エラーを返す
  （`IMG-3` 到着前は、そのノードがまだ無いことも書く）。
- `Scene::holds_gpu_resident` / `byte_size` / `collect_flat` の `match` から
  画像腕を落とす。
- `docs/agent-api-reference.md` の `SceneContent` / `FlatContent` /
  `SceneImage` の記述（780・786・805–806 行付近）と `scene.add` の行
  （1122 行付近）を実装に合わせる。
- `docs/implementation/3d-scene-plan.md` 単位 4 の「FrameBuffer オブジェクトは
  テクスチャ付き矩形として描く」と、その完了条件の FrameBuffer 2 項目を、
  「画像を持つ instance source を描く」形へ書き換える。**UV 属性でサンプリング
  する（quad 専用の暗黙 UV にしない）という制約は残す。**

**依存**: なし。

**完了条件**

- `SceneContent` / `FlatContent` に画像バリアントが存在しないこと。
- `scene.add` に FrameBuffer を繋ぐと、`geometry.from_image` を案内する明示エラーに
  なるテスト（黙って素通しにも panic にもしない）。
- 既存の Scene テスト（入れ子の平坦化、`byte_size`、`is_gpu_resident`）が
  ジオメトリだけで書き直されて通ること。
- `mise run docs:check` が通り、`agent-api-reference.md` と
  `3d-scene-plan.md` に画像オブジェクトの記述が残っていないこと。
- REQ-3D-001 の本文（「オブジェクトはジオメトリまたは FrameBuffer」、
  受入条件の FrameBuffer 3 項目、既知の制約の「複製できない」）を
  ジオメトリ経由の表現へ修正し、`docs/requirements/overview.md` との整合を
  確認すること。**この単位が REQ-3D-001 本文の唯一の書き換え箇所**である。

### `IMG-2`: `InstanceSource` への一般化（`ravel-core`、挙動不変）

- `crates/ravel-core/src/geometry/` に `InstanceSource` を追加し、
  `Geometry::instance_sources` の要素型を差し替える。
  `Arc<dyn NodeData>` はデバッグ表示を持たないので、`SceneImage` と同じく
  **`Debug` は手書き**にする（`Geometry` の `#[derive(Debug)]` を壊さない）。
- 画像側は `frame: Arc<dyn NodeData>` と `width` / `height` を持ち、
  構築時に `DataTypeId::FRAME_BUFFER` と非ゼロ解像度を検査する
  （`SceneImage::new` の検査をそのまま移す）。
- `instance_source()` / `set_instance_source()` / `set_instance_sources()` の
  既存シグネチャは `Geometry` 用の便宜として残し、`InstanceSource` 版を足す。
  呼び出し側（`scatter/mod.rs`、`geometry.rs`、`attribute/mod.rs`、
  `rasterize/mod.rs` の計 8 ファイル）が**ソースの中身を見ずに**扱えることを
  この単位で確認する。
- `Geometry::byte_size` に画像のバイト数を足す（現在は
  `instance_sources` を再帰して合算しているので、そこに腕が 1 つ増えるだけ）。
- **`Geometry` に `NodeData::is_gpu_resident` の override を足す。**
  現在の `impl NodeData for Geometry`（`container.rs:534-560`）は
  `data_type_id` / `as_any` / `byte_size` の 3 つしか持たず、
  既定実装の `false` を返している。画像を抱えた時点でそれは嘘になる。
  `Scene::holds_gpu_resident` と同じ形で instance source を再帰する。
- **`Geometry::byte_size` を飽和加算にする。** 現在は素の `+` と
  `sum::<u64>()` で、`Scene::byte_size` だけが `saturating_add` を使っている。
  この単位で `Arc<dyn NodeData>`（実装が任意の見積りを返せる型）が
  `instance_sources` に入るので、**`Scene` が飽和で守っていた性質が
  `Geometry` 側で必要になる**。`IMG-1` が偽 `NodeData` の注入点を
  `Scene` から失った分は、ここで戻る
- この単位では**まだ誰も画像を作らない**。挙動は変わらない。

**依存**: なし（`IMG-1` への技術的な依存は無く、決定 9 の順序ゲートは
フェーズ C4 の完了で解けた）。

**完了条件**

- 既存のジオメトリ・スキャッタ・ラスタライズのテストが**1 つも変更なしで**
  通ること（挙動不変の担保）。
- 画像を持つ `Geometry` の `byte_size` が画像のバイト数を含むテスト。
- GPU 常駐フレームを持つ `Geometry` の `is_gpu_resident` が `true` に、
  CPU フレームだけなら `false` になるテスト。入れ子の instance source
  越しにも伝播するテスト。
- **入れ子の `Scene` 越しにも伝播するテスト**（常駐画像を持つ `Geometry` を
  入れ子 Scene に置くと `Scene::is_gpu_resident` が `true` になる）。
  `Scene::holds_gpu_resident` の再帰は `IMG-1` の時点で**到達不能**になって
  おり、誰かが `false` に潰しても検知できない状態で残っている。
- **敵対的な `byte_size` を返す画像を入れても飽和し、debug panic しない**
  テスト（`IMG-1` が `Scene` から失ったカバレッジの戻り先）。
- 画像ソースを構築するときに**リードバックが起きない**テスト
  （`GpuFrameBuffer` の先例と同じ検証形）。
- 非 FrameBuffer / 解像度ゼロを渡すと型付きエラーになるテスト。

### `IMG-3`: `geometry.from_image` ノード

- FrameBuffer → Geometry。出力は決定 3 の形（インスタンス 1 個、
  `P = (0,0)`、`index = 0`、`instance_sources = [Image]`、
  points / primitives は空）。
- 矩形はソースのピクセル寸法、原点中心（決定 5）。パラメータは持たない。
- CPU / GPU どちらの表現で来ても**変換せずに**包む（決定 6）。
- 登録は `docs/dev/add-node.md` のチェックリストに従う
  （`registry/builtin.rs` のテンプレート、`processor_for_node` の `match`、
  ロケール、アイコン）。
- **`type_key` は `geometry.from_image`**（2026-08-06 決定）。`docs/dev/add-node.md`
  の `<領域>.<名前>` 規約で、既存の綴りは**出力ドメイン**を領域に取っている
  （`shape.*` / `scatter.*` / `geometry.*` → Geometry、`field.*` → Field、
  `scene.add` → Scene、`comp.*` → FrameBuffer）。このノードは Geometry を出すので
  領域は `geometry`。入力側を領域に取る `framebuffer.to_geometry` は規約と逆向きで、
  `framebuffer.*` という領域も新設になるため採らない。
  **永続化に載る識別子なので、実装後に変えると移行が要る。**
- **`scene.add` の誘導メッセージから未実装の節を落とす。** `IMG-1` は
  FrameBuffer を繋いだユーザーに「`geometry.from_image` を挟め — ただし
  このビルドにまだ無い」と返している。このノードが入った時点でその後半は
  偽になる。**テストが `does not have yet` を assert しているので、
  文言を放置しても失敗しない**（変えたときだけ落ちる）— だからここに書く。

**依存**: `IMG-2`。

**完了条件**

- 出力の instance 数が 1、`instance_sources` が 1 要素で画像、
  points / primitives が空であるテスト。
- 出力を `scatter.grid` の `instance_source` ポート（`DataTypeId::GEOMETRY`
  のまま）に繋ぐと、深さ 2 の入れ子になり `MAX_INSTANCE_DEPTH` に触れない
  テスト。
- CPU フレームと GPU フレームの両方を受け、**表現が変わらない**テスト。
- `Geometry::validate` を通ること（instance ドメインの `P` 必須）。
- ロケール（`en.toml` / `ja.toml`）に `label` があるテスト、または
  既存のロケール網羅テストが通ること。

### `IMG-4`: `rasterize` のテクスチャ経路（CPU 参照）

CPU 経路はゴールデンテストのオラクルなので先に入れる
（`gpu-resident-geometry-plan.md` の「CPU 経路は参照実装として残す」と同じ規律）。

- `ensure_planar_paths` の instance source 再帰から画像を外す（決定 8）。
- `raster_instances`（`rasterize/mod.rs:690`）で選ばれたソースが画像なら、
  再帰せずに矩形を描く。合成後の `Placement`（offset / rot / scale / tint）の
  逆変換で出力ピクセルからソース texel を引き、バイリニアで標本化して
  src-over でブレンドする。**描画順は index 順のまま**（決定 7）。
- CPU 参照経路は CPU のピクセルを要求するので、GPU 常駐フレームが来たら
  ここでだけ読み戻す（`crate::ensure_cpu`）。**本番の GPU 経路（`IMG-5`）は
  読み戻さない。** この非対称は明示的な制約として文書に残す。

**依存**: `IMG-2`、`IMG-3`。

**完了条件**

- 1 枚を原点に描いたとき、出力が元画像とピクセル一致するゴールデンテスト
  （等倍・無回転）。
- 格子に並べたときの位置・スケール・回転が期待どおりのゴールデンテスト。
- `Cd` × `alpha` の tint が texel に掛かるテスト。
- 重なった 2 枚で index の大きい方が上に来るテスト（決定 7 の pin）。
- `source_index` で 2 枚の画像を出し分けるテスト（コンタクトシート）。
- **メッシュを含むジオメトリは従来どおり拒否される**テスト
  （決定 8 の pin。画像を通したせいでガードが緩んでいないことの回帰）。
- 拡大時にボケることを**期待挙動として固定する**テスト、または
  仕様書への明記（決定 5）。

### `IMG-5`: `rasterize` のテクスチャ経路（GPU）

- `flatten_geometry` が画像インスタンスを `DrawItem` に落とす。
  種別は `data0[0]` の既存の判別（1.0 = パス、0.0 = 点スプライト）に
  3 つめを足す。矩形の 4 隅と逆変換に要る値を `data0` / `data1` に載せる。
- **テクスチャの束縛方法がこの単位の設計上の難所**である。現在の
  `RasterPipeline` はバインディング 3 本（uniform + storage 2 本）で
  テクスチャを 1 枚も持たない。順序（決定 7）を壊さずに複数ソースを扱う
  必要があるため、実装時に次から選ぶ:
  - (a) テクスチャ配列 + `DrawItem` にレイヤ index。**ソースの解像度が
    揃っている必要がある**ので、そのままでは一般解にならない
  - (b) アトラス。詰め替えのコストとサイズ上限を負う
  - (c) **ソースが変わる境目で描画を分割する**（index 順を保ったまま
    連続する同一ソースの塊ごとにパスを分ける）。順序が構造的に保たれ、
    画像の枚数が少ない前提では最も安い。**まずこれで入れる**
- WGSL 側はフラグメントで配置行列の逆変換 → UV → サンプリング。
  既存のプレマルチプライ済み合成と `unpremultiply` パスの規約は変えない。

**依存**: `IMG-4`。

**完了条件**

- `IMG-4` のゴールデンテストが GPU 経路でも許容誤差内で一致すること
  （既存の `gpu_matches_cpu_for_paths_points_and_nested_instances` と同じ形）。
- 重なり順が CPU と一致するテスト（分割描画で順序が崩れていないことの pin）。
- **GPU 常駐フレームを入力にしたときリードバックが起きない**テスト
  （`ravel_gpu::transfer::stats` の `read_texture` 計数）。
- 複数ソースの出し分けが GPU でも効くテスト。
- GPU アダプタが無い CI ではスキップされるので、**アダプタありの実機確認を
  マージ条件に含める**（`gpu-resident-geometry-plan.md` と同じ扱い）。

### `IMG-6`: レジストリ / ロケール / 文書

- `docs/specifications/procedural-geometry.md` に instance source の
  種別（ジオメトリ / 画像）と、**解像度 = コンポ単位・拡大でボケる**規約
  （決定 5）を書く。`rasterize` の受け入れ表に画像インスタンスを足す。
- `docs/agent-api-reference.md` に `InstanceSource` と `geometry.from_image` を足す。
- `docs/ui-impl-status.md` に、画像を並べる操作が使えるようになったことを
  反映する。
- ロケールの `description` と `params.*` を埋める。
- 本計画書の Status を更新し、`backlog.md` の行を `✅` にする。

**依存**: `IMG-1`〜`IMG-5`。

**完了条件**

- `mise run docs:check` が通ること。
- 仕様書に「拡大するとボケる」と「コピーごとの再評価はしない」が
  明記されていること（決定 5 / 決定 1 が後から蒸し返されないための pin）。

## 検証

| レベル | 何を | どこで |
|---|---|---|
| `ravel-core` ヘッドレス | `InstanceSource` の構築検査、`byte_size`、`is_gpu_resident` の伝播、リードバックが起きないこと | `crates/ravel-core` の単体テスト |
| `ravel-nodes` ヘッドレス | `geometry.from_image` の出力形、`scatter` との合成、深さガード、`source_index` の出し分け | `crates/ravel-nodes` の単体テスト |
| ゴールデン | 1 枚 / 格子 / 重なり / tint / 拡大時のボケ | `rasterize` の既存ゴールデン形式（CPU 経路が基準） |
| CPU / GPU 一致 | `IMG-4` の全ゴールデンを GPU 経路で再実行 | 既存の一致テストと同じ形。**アダプタありの実機確認が必須** |
| GPUI | 不要 | 本計画は UI を持たない。ノードの登録・ロケールは既存の網羅テストが拾う |

**回帰の要点は「挙動不変であるはずの単位が本当に挙動不変か」**である。
`IMG-1` と `IMG-2` は既存テストを 1 行も変えずに通ることを条件にしてある。

## 非対象

決定メモが明示的に却下 / 先送りしたもの。**本計画のどの単位もこれらを含まない。**

- **コピーごとのネットワーク再評価**（時刻オフセット、パラメータ変化）。
  決定 1。`per-instance-modulation-plan.md` / `evaluation-scope-plan.md` の
  「per-instance subgraph re-evaluation は導入しない」を越えない
- **コピーごとのブレンドモード**（add / multiply / screen）。決定 7
- **変形**: コーナーピン、ワープ、ページカール、3D 射影、および
  それらが要求する `to_mesh`（UV 付きメッシュ）。決定 8。`scene.render` の管轄
- **`Primitive` への画像バリアント追加と UV 標準属性の追加**。決定 3
- **`resolution_scale`**（上流を高解像度で再評価する係数）。決定 5
- **CPU への正規化 / 画像レジストリ**。決定 6
- **`rasterize` のメッシュ拒否の撤廃**。決定 8。ガードは残す

## 未解決の依存

### `gpu-resident-geometry-plan.md` の `GPU-5` — 前提が変わる

`GPU-5`（`rasterize` が常駐ジオメトリを直接読む）は
**「属性列だけを GPU に置き、`Primitive` と `instance_sources` の入れ子は
CPU 側メタデータのまま残す」**という前提で書かれている。同計画の非対象にも
「トポロジの GPU 化。`Primitive` と `instance_sources` は CPU 残留」とある。

画像インスタンスが入ると、`instance_sources` は**数値列でもトポロジでもない
第 3 の種別（テクスチャハンドル）**を持つ。これは「CPU 残留」で済まない —
描画時に GPU 側でサンプリングされる必要がある。

**影響**:

- `GpuGeometry` のスケッチにある `instance_sources: Vec<Arc<Geometry>>` は
  そのままでは足りない。画像ソースは**アップロードの対象ではなく
  既にテクスチャである**（決定 6 で常駐のまま持つため）ので、
  「属性列をアップロードする」経路とは別扱いになる
- `GPU-5` の「GPU 側で 1 段目までを直接展開し、2 段目以降は CPU 展開に落とす」
  という段階導入は、画像ソースがどちらの段に現れるかで難易度が変わる。
  `geometry.from_image` → `scatter` の典型形では画像は**2 段目**に現れる
- `IMG-5` のテクスチャ束縛方式（上記 (a) / (b) / (c)）は `GPU-5` の
  ドローコール構造に直接影響する

**どちらが先でもよいが、後から入る方が相手の形を読むこと。**
本計画は `GPU-5` に依存しないし、`GPU-5` を待たない。

### `cache-plan.md` の `CACHE-3` — VRAM 会計に穴が開く

決定 6 の代償。`GpuFrameBuffer` は `Arc<PooledHandle>` で、最後のクローンが
落ちるまでテクスチャがプールへ返らない。**画像を抱えた `Geometry` は
VRAM を握り続ける。**

`CACHE-3` は `NodeData::byte_size()` と `CacheBudget`（層別会計）を入れ、
`TexturePool` の `LruBudget` を `CacheBudget` に従属させる単位である。
そこに次が要る。

- 画像付き `Geometry` を **VRAM 層に計上する**。`IMG-2` が
  `Geometry::is_gpu_resident` を実装するので判定材料は揃うが、
  **`Scene` と同じ「1 値 1 層」の粗い計上でよいか**は `CACHE-3` の判断。
  `Scene` は「CPU ジオメトリと常駐テクスチャが混ざったら全額 VRAM」という
  過大計上を意図的に選んでいる（`scene/mod.rs:424-437` の doc）ので、
  `Geometry` も同じ規約に揃えるのが素直
- 同じフレームを N 個の instance source が共有した場合、`byte_size` は
  `Arc` を**持ち主ごとに 1 回**数える既存規約に従う（`Scene::byte_size` の
  コメントと同じ）。過大計上だが、退避が早まるだけで壊れない
- 退避の効きが変わる: 画像付きジオメトリを退避しても、**他のクローンが
  生きている限りテクスチャは返らない**

**`IMG-2` は `CACHE-3` を待たない**（`byte_size` と `is_gpu_resident` は
`CACHE-3` 以前から `NodeData` にある）。逆に **`CACHE-3` が本計画より後に
入る場合、その設計は画像付きジオメトリを最初から勘定に入れること。**

### `docs/requirements/REQ-3D.md` の REQ-3D-001 — 本文の書き換えが要る

決定 4 が要件の記述と食い違う。**本計画書はリンクを置くだけで、本文は
書き換えない。** 書き換えは `IMG-1` の完了条件に含めてある
（要件変更は実装単位が入るときに行う）。

書き換えの対象:

- 説明の「オブジェクトは**ジオメトリ（REQ-CORE-010）または FrameBuffer**」
- 受入条件の「FrameBuffer をオブジェクトとして追加でき、テクスチャ付き矩形になる」
  ほか FrameBuffer 関連の 3 項目
- 既知の制約「**FrameBuffer オブジェクトは複製できない**」— 本計画が
  解消する側になる。「Mesh へのテクスチャ割り当て」と「FrameBuffer
  オブジェクトのインスタンス化」のどちらを取るかという保留も、
  決定 2 / 決定 3 で決着している

併せて `docs/requirements/overview.md` の該当行を確認すること。

### `geometry-ops-plan.md` / `path-channel-design.md` の instance 規約

- `geometry-ops-plan.md` の `OPS-1`（`geometry.blast`）は
  「インスタンスドメインの削除では `instance_sources` の参照も整理」と書く。
  `IMG-2` の一般化後も**中身を見ずに**扱えるので記述は成り立つが、
  実装時に画像ソースを落としそこねると VRAM が残る
- `OPS-7`（`geometry.repeat`）は「元ジオメトリを `instance_source` に置く」。
  画像を持つ `Geometry` を入力にすると深さが 1 増える（`geometry.from_image` →
  `repeat` → 深さ 2）。`MAX_INSTANCE_DEPTH = 4` の範囲内
- `path-channel-design.md` は instance ドメインに触れないので影響しない

いずれも**記述の書き換えは不要**。`IMG-2` の実装時に、これらの単位が
まだ入っていなければ何もしなくてよい。

## 未解決の問い（実装時に決める）

- ~~**`FlatContent` を 1 バリアントの enum として残すか、`Arc<Geometry>` に
  潰すか**（`IMG-1`）~~ → **潰した**。`FlatObject` は
  `{ geometry: Arc<Geometry>, world_transform: Mat4 }`。決定 4 の後、
  「`SceneContent` から入れ子を除いたもの」は定義上ジオメトリ 1 種であり
  （画像は instance source の中、Mesh と Path は `Primitive` の中）、
  1 バリアントの enum は `3D-4` の全消費者に無意味な分解を強いる。
  ライトは `SceneObject` ではなく `Scene` のライト列に載る予定なので、
  この enum の拡張点にはならない。2 種目の描画対象が現れたら enum を
  戻す — 消費者ゼロの今も `3D-4` 着手時も、その差し替えの費用は同じ。
- ~~**`IMG-5` のテクスチャ束縛方式**（(a) 配列 / (b) アトラス / (c) 分割描画）~~
  → **(c) で入った**。`ravel-gpu` 側は `QuadDraw.instance_count` を
  `QuadDraw.runs: &[QuadRun]` に置き換えただけで、レンダーパスは 1 本のまま
  run ごとに bind group を差し替える。**再検討の引き金は「1 フレームに現れる
  異なる画像ソースの数」**であって、コピーの数ではない（同じ絵の N コピーは
  隣接すれば 1 run に畳まれる）。ソースが数十を超えるようなら (b) アトラス。
- ~~**`Geometry::instance_sources()`（geometry 専用の便宜ビュー）を残すか**~~
  → **消した**。production の呼び出し元は `IMG-2` の時点で全て `sources()`
  へ移っており、残っていたのは**テストだけ**だった。画像を黙って落とし、
  `source_index` との対応も崩れる accessor を、次に production コードを書く
  人が拾える場所へ置いておく理由が無い。単数形の `instance_source()` は
  残してある — こちらは index を持たないので誤対応が起こらず、最初のソースが
  画像なら `None` を返して黙らない。
