# パスのシェーディング — 頂点色補間と `stroke_align`

**要件**: REQ-MOGRAPH-001（要素ごとの見た目）、REQ-RENDER-001（CPU / GPU の
出力一致）、REQ-CORE-012（属性駆動）

**関連する票**: `issues/medium/gpu-nodes.md` の `MED-GPU-08`

## 問題

`rasterize` の CPU 経路と GPU 経路が**per-pixel の幾何情報を持っているか**で
非対称になっており、それを必要とする機能が 3 つ、別々の理由で止まっている。

### 止まっているもの

| 機能 | どこで止まったか | 必要な per-pixel 情報 |
| --- | --- | --- |
| **Point ドメインの色（頂点色補間）** | `MED-GPU-08`。`STYLE-6` の完了条件を 1 つ落とした | 最近傍セグメントの index と、その上の位置 `t` |
| **`stroke_align`（内側 / 外側）** | `style-attributes-plan.md` 単位 1 → 単位 3 へ繰り延べ | **符号付き**距離（内外の別） |
| **ダッシュ**（GPU 側） | 同 単位 3。こちらは**鏡像の問題** | パス始点からの弧長 |

### 非対称の中身

**GPU** は画素ごとにポリラインを走査して、距離と巻き数をその場で出している
（`crates/ravel-nodes/src/shaders/rasterize.wgsl` の `path_coverage`）:

```wgsl
for (var i = 0u; i < segment_count; i += 1u) {
    min_distance = min(min_distance, segment_distance(p, a, b));
    // winding も同じループで数える
}
```

**必要な情報はすでにこのループの中にある。** `min` を取るときに index と
`t` を一緒に持ち回れば頂点色補間になり、`winding` の符号を距離に掛ければ
`stroke_align` になる。**GPU 側はどれも数行**。

**CPU** は `zeno` に丸投げしていて、返ってくるのは 0..255 の被覆マスクだけ
（`crates/ravel-nodes/src/rasterize/mod.rs`）:

```rust
Mask::new(commands.as_slice())
    .size(width, height)
    .style(Fill::NonZero)
    .render_into(canvas.coverage, None);
canvas.blend_coverage(coverage_rect(...), color);   // ← 1 色で塗る
```

`blend_coverage` は矩形を舐めて被覆値 1 つと色 1 つを混ぜる。
**「この画素がどの頂点の間にあるか」も「内側か外側か」も出せない。**

### なぜ「近似で埋める」が効かないか

`RESP3-12` 以降、**ゴールデンは「CPU と GPU が許容誤差内で一致すること」を
検査する**（`crates/ravel-nodes/tests/shape_layer_golden.rs`）。片側だけを
近似で実装すると、その一致そのものが壊れる。`stroke_align` を単位 1 から
繰り延べた判断がまさにこれで、「2 倍幅ストローク × 塗りカバレッジ」で
内側を近似すると境界画素が GPU の解析的な符号付き距離と 0.25 程度ずれた。

**だから 3 つとも「CPU 側の per-pixel 評価をどうするか」1 つの判断に依存して
いる。** 別々に決めると、同じ議論を 3 回して 3 通りの答えを出すことになる。

## 目標アーキテクチャ

**CPU にも、GPU シェーダと同じ per-pixel 評価器を置く。** 同じ式を 2 つの
言語で書くのではなく、**同じ規則を 1 か所に書いて両方から呼ぶ**形にする。

```text
                      ┌─ path_coverage()  … WGSL（既存。情報を足すだけ）
polyline + 画素 p ────┤
                      └─ path_sample()    … Rust（新規。CPU 経路が呼ぶ）

  どちらも返す: min_distance / winding / nearest_segment / t_on_segment
```

### 3 つの重要な設計上の縛り

1. **被覆（アンチエイリアス）は zeno のまま。** 新しい評価器が返すのは
   **色を決めるための情報だけ**で、どれだけ濃いかは今までどおり zeno の
   マスクが決める。こうしないと既存ゴールデンが全部動く。
   `stroke_align` だけは被覆そのものを変える必要があるので例外（後述）
2. **既定の経路は 1 画素も遅くしない。** per-pixel のセグメント走査は
   **頂点色を持つパスにだけ**走らせる。`Cd` の Point 列が無いパスは
   今の `blend_coverage`（被覆 1 つ・色 1 つ）をそのまま通る
3. **一致はテストで確かめるのではなく、構造で担保する。** 同じ規則を両側で
   使い、ゴールデンは**その一致が崩れていないこと**を検査する側に回す

### 塗り（fill）の扱いが最大の分岐

**線（stroke）は最近傍セグメント + `t` で意味が確定する** — ストロークとは
ポリラインの近傍そのものなので、「どの頂点の間か」に曖昧さが無い。

**塗りの内部には、境界の頂点色から補間する自然な規則が無い。** 最近傍境界
セグメントで塗ると Voronoi 状の領域分割になり、ユーザーが「グラデーション
塗り」に期待する絵とは違う。期待されているのは三角形分割 + 重心座標
（Gouraud）に近いもの。

**ロードマップがフェーズ D の完成形として掲げているのは線の方**である
ことに注意する:

```text
線に沿ったグラデーション
  shape.line → attribute.curveu → field.attribute("u")
    → field.ramp → field.apply("Cd") → rasterize
```

`shape.line` は**開いたパス**なので塗りが無い。**線だけ実装すれば
ロードマップの約束は果たせる。**

## CPU 側の方式 — 選択肢（**未決定。判断を残す**）

per-pixel 評価器そのものは上のとおりだが、**塗りをどう扱うか**が決まって
いない。3 案あり、どれを採るかで単位の数と実装コストが変わる。

### 案 A: 線だけ。塗りはプリミティブ色のまま（推奨）

- 最近傍セグメント + `t` で線の色を補間する。塗りは今までどおり
  `Cd` のプリミティブ値 1 色
- **ロードマップの約束（線に沿ったグラデーション）をそのまま満たす**
- 塗りに Point ドメインの `Cd` を書いた場合の扱いを決める必要が残る
  （無視 / 平均 / 先頭 — これも判断が要る）
- 実装は CPU / GPU とも小さい。新しい依存なし

### 案 B: 塗りも最近傍境界セグメントで補間する

- 案 A と同じ情報で塗りも色付けできる（追加コストほぼゼロ）
- **出る絵が期待と違う**。星形や凹多角形で領域が割れて見える
- 「効かない」よりはマシだが、「効くが変」は説明が要る状態を作る

### 案 C: 塗りを三角形分割して重心座標で補間する

- 期待どおりの Gouraud 塗りになる
- **三角形分割器はすでにある。** `PATH-0b`（#301）が `earcut` の採用を決めて
  あり、`crates/ravel-core/src/geometry/triangulate.rs` の `Triangulator` が
  穴つき多角形を扱い、フレーム間でスクラッチバッファを再利用する。
  `Geometry` 側にも `Primitive::Mesh` と `push_mesh`、共有インデックス
  バッファが揃っている。**新しい依存も新しいアルゴリズムも要らない**
- **代わりに要るのは `rasterize` の三角形描画経路。** 現在の `rasterize` は
  **メッシュを明示的に拒否する**（`ensure_planar_paths`、
  `crates/ravel-nodes/src/rasterize/mod.rs:397-409`。テスト
  `mesh_primitives_are_an_explicit_error` が pin している）。CPU も GPU も
  三角形を描く道を持っていない
- GPU 側は三角形を送る形になるので `DrawItem` のバッファ構造が変わる
- 被覆は zeno のまま、色だけ三角形から引く、という形にはできる
  （画素ごとに含む三角形を引いて重心座標を出す）
- **`3D-4`（三角形レンダラと `scene.render`、着手可能）と重なる。** 先に
  そちらが入るなら、この経路はその上に乗せられる可能性がある

**推奨は A**。理由は「三角形分割器を抱えたくない」ではなく（それはもう
ある）、**`rasterize` に三角形描画経路が無いこと**と、ロードマップが要求して
いるのが線であること。

**この判断は保留する。** 単位 1 に入る前に決める。C を採るなら
**`3D-4` との順序**も一緒に決めること。

### この 3 案は「普通のグラデーション塗り」の話ではない

上の A / B / C は**頂点色**（トポロジ由来の色）をどう面に広げるかの議論で、
**位置由来のグラデーション塗り**（線形 / 放射、AE の Gradient Ramp、
Illustrator や Figma の普通のグラデーション）とは別物。混同しないこと。

位置由来のグラデーションには per-pixel の幾何情報が要らない — 要るのは
画素の座標だけで、CPU（`blend_coverage` の走査）も GPU
（`@builtin(position)`）も既に持っている。したがって**この計画の
per-pixel 評価器にも三角形分割にも依存しない**。

- **フレーム全体に 1 枚**でよいなら `effects-library-plan.md` の **`FX-3`**
  （`comp.gradient`、🟡 着手可能）が post-process で覆う。rasterize への
  変更はゼロ。AE と同じ形
- **要素ごとに別のグラデーション**が要るなら rasterize の中でやるしかない。
  それが `PSHADE-6`（下記）

## 実装単位

| 単位 | 内容 | 依存 |
| --- | --- | --- |
| `PSHADE-1` | `path_sample()`（CPU の per-pixel 評価器）と WGSL 側の情報追加。**挙動不変** | — |
| `PSHADE-2` | 線の頂点色補間（CPU / GPU）。`MED-GPU-08` の本体 | `PSHADE-1`、**塗りの方式決定** |
| `PSHADE-3` | `stroke_align`（標準属性の宣言 + CPU / GPU 両経路） | `PSHADE-1` |
| `PSHADE-4` | 塗りの頂点色（案 B か C。**案 A を採るなら単位ごと落とす**。C なら `3D-4` の三角形レンダラとの順序も決める） | `PSHADE-2` |
| `PSHADE-5` | ゴールデンの拡張と文書。`MED-GPU-08` を閉じる | `PSHADE-2`, `PSHADE-3` |
| `PSHADE-6` | **要素ごとのグラデーション塗り**（位置由来。線形 / 放射） | `FX-3`（先に post-process 版を出す）、`STYLE-2` |

`style-attributes-plan.md` 単位 3 の `stroke_align` は `PSHADE-3` が引き取る。
**ダッシュ（GPU 側の弧長）は単位 3 に残す** — 弧長は per-pixel ではなく
セグメント単位の前計算で足りるので、この計画の per-pixel 評価器とは別の話。

## 単位ごとの完了条件

### `PSHADE-1` per-pixel 評価器（挙動不変）

- `path_sample(polyline, closed, p) -> PathSample` が
  `{ min_distance, winding, nearest_segment, t_on_segment }` を返す。
  置き場所は `crates/ravel-nodes/src/rasterize/`
- WGSL の `path_coverage` が同じ 4 つを返すよう拡張される
- **どちらの呼び出し元も、今と同じ被覆と同じ色を出す。** 情報を足すだけで
  使わない
- 次を落とすテストがある:
  - `path_sample` と WGSL が同じ入力で同じ `min_distance` / `winding` を返す
    （代表的な画素で。GPU が使えない環境では CPU 側だけ動かして
    ゴールデンに委ねる）
  - 退化入力: 長さ 0 のセグメント、頂点 1 個、自己交差、閉じたパスの
    始点ちょうど
- **既存ゴールデンが 1 枚も変わらない**

### `PSHADE-2` 線の頂点色補間

- Point ドメインに `Cd` があるパスは、線の色が**最近傍セグメントの
  両端の色を `t` で補間したもの**になる
- **`Cd` の Point 列が無いパスは今の経路をそのまま通る**（per-pixel 走査を
  しない）。これを計測で示す
- `stroke_color` も同じ規則に乗る（Point 列があればそちらが優先）
- 次を落とすテストがある:
  - `shape.line → attribute.curveu → field.attribute("u") → field.ramp
    → field.apply("Cd") → rasterize` で、**線の始点側と終点側の画素の色が
    異なる**ゴールデン（`STYLE-6` が属性レベルまでしか確かめられなかったもの）
  - 同じ経路の CPU / GPU が許容誤差内で一致する
  - `Cd` の Point 列が無いときの絵が**無改変**である
  - 頂点色があっても `stroke_width = 0` なら何も描かれない

### `PSHADE-3` `stroke_align`

- 標準属性表に `stroke_align`（Primitive、I32、0=中央 / 1=内側 / 2=外側）を
  宣言する。**宣言と実装を同じ単位で入れる**（`style-attributes-plan.md`
  単位 1 が「あるのに効かない」を避けるために宣言を見送った経緯を守る）
- CPU は `path_sample` の符号付き距離で被覆を作る。**ここだけは zeno の
  マスクを使わない**（整列は被覆そのものを変えるため）
- 次を落とすテストがある:
  - 中央 / 内側 / 外側で、線の内外の広がりが期待どおりに変わるゴールデン
  - **CPU / GPU が許容誤差内で一致する**（繰り延べの理由そのもの）
  - `stroke_align` 未設定のとき既存の絵が**無改変**である

### `PSHADE-4` 塗りの頂点色（採否は保留）

案 B を採るなら:

- 塗りの色が最近傍境界セグメントの補間になるテスト
- 凹多角形で領域が割れることを**仕様として** pin するテスト

案 C を採るなら:

- `rasterize` がメッシュを拒否しなくなること。`mesh_primitives_are_an_explicit_error`
  が pin している現在の挙動を**意図的に**変えることになるので、単位の冒頭で
  そう宣言する
- 既存の `Triangulator`（`PATH-0b`、#301）を使い、**新しい三角形分割器を
  書かない**。退化入力（凹・自己交差・穴）はその型の既存テストが覆う
- 重心座標の補間が期待値になるゴールデン
- CPU / GPU が許容誤差内で一致する

### `PSHADE-5` ゴールデンと文書

- CPU / GPU 等価ゴールデンを頂点色と `stroke_align` 込みで拡張する
- `docs/specifications/procedural-geometry.md` の標準属性表を更新する
- `docs/agent-api-reference.md` に `path_sample` を載せる
- `issues/medium/gpu-nodes.md` の `MED-GPU-08` を閉じ、
  `issues/closed/medium-gpu-nodes.md` へ移す
- `style-attributes-plan.md` 単位 6 の注記（完了条件を属性レベルに直した
  経緯）を、実装済みの記述に直す

### `PSHADE-6` 要素ごとのグラデーション塗り

**`FX-3`（`comp.gradient`）の後に置く。** フレーム全体に 1 枚のグラデーションを
掛けるだけなら post-process で足り、rasterize を触る理由が無い。この単位が
存在する理由は**要素ごとに違うグラデーションを掛けられること**、それだけ。
scatter した 50 個の図形が別々のグラデーションを持てるのは post-process では
できない。

#### 座標系 — 選択の余地が無い部分

`Placement` は**非一様スケール → 回転 → 平行移動**
（`crates/ravel-nodes/src/rasterize/mod.rs:66-99`）。これは**アフィンだが
相似変換ではない**。

したがって「軸の 2 端点を device 空間へ変換して、そこで射影する」は**誤り**。
線形グラデーションの等値線は軸に垂直な直線だが、非一様スケールと回転の合成は
垂直性を保たないので、`scale.x != scale.y` かつ `rot != 0` のとき等値線の向きが
ずれる。放射も同様で、円が楕円に写る事実を device 空間では表現できない。

**ジオメトリ空間で評価する。** 各 `DrawItem` が `Placement` の**逆変換**
（逆 2×2 と平行移動で 6 float）を持ち、画素をジオメトリ空間へ戻してから
グラデーションを引く。

- 任意のアフィンで厳密。放射が非一様スケール下で**正しい楕円になる**のは
  この副産物
- CPU と GPU が**同じ空間で同じ式**を使うので、一致が構造で担保される
  （この計画の縛り 3）
- コストは画素あたり 2×3 の積 1 回

**既知の隣接する近似**: 線幅は `uniform_scale()`（`|sx| + |sy| / 2`）で
スケールしており、非一様スケール下では既に近似になっている（`:100-102`）。
**この単位はそれを直さない。** 直すなら別単位で、`stroke_align`（`PSHADE-3`）と
同じ「線の幾何を per-pixel でやり直す」話に合流する。

#### ランプの置き場所 — ノードパラメータ。属性ではない

`AttributeArray` は `F32` / `Vec2` / `Vec3` / `Vec4` / `Color` / `I32` /
`Bool` / `Str` の 8 種で、**可変長の構造を持てない**
（`crates/ravel-core/src/geometry/attribute.rs:116-125`）。ランプ属性を作るには
属性の種類そのものを増やすことになり、永続化・GPU アップロード・
スプレッドシート表示のすべてに波及する。**この単位ではやらない。**

したがって v1 は **1 ノード 1 ランプ**（`ParameterValue::Ramp`、#408 で追加済み）。
**要素ごとに変わるのは軸**。これは実用上ちょうどよい分割で、「同じ配色を
それぞれの図形の向きに合わせて掛ける」が素直に書ける。

複数ランプを選び分けたくなったら、ノードがランプの配列を持ち `fill_ramp`
（Primitive、I32）が索引する形で後から足せる。**v1 には入れない。**

#### 軸 — Primitive ドメインの標準属性

| 属性 | ドメイン | 型 | 意味 |
| --- | --- | --- | --- |
| `fill_gradient_start` | Primitive | Vec2 | ジオメトリ空間の始点。放射では中心 |
| `fill_gradient_end` | Primitive | Vec2 | 同終点。放射では半径を決める点 |
| `fill_gradient_type` | Primitive | I32 | 0 = 線形 / 1 = 放射 |

- 線形: `t = clamp(dot(p - start, end - start) / |end - start|^2, 0, 1)`
- 放射: `t = clamp(|p - start| / |end - start|, 0, 1)`
- `|end - start| == 0` は**先頭ストップの単色**（ゼロ除算にしない。
  `field.ramp` の `in_min == in_max` と同じ扱い）

**属性が無いときの既定**: その要素の bbox の左中央 → 右中央。
設定ゼロで「横方向のグラデーション」が出る。**角度パラメータは作らない** —
向きは軸属性で言えるので、同じことを 2 通りで言えるようにしない。

Vec2 属性なので `field.apply` でそのまま駆動でき、要素ごとの向きを
フィールドで作れる。これがこの単位の狙いそのもの。

#### 頂点色との優先順位

位置由来（この単位）とトポロジ由来（`PSHADE-2` / `PSHADE-4`）は独立した
仕組みで、両方の指定が同時に存在しうる。

- **線**: 頂点色（`PSHADE-2`）が勝つ
- **塗り**: グラデーション（この単位）が勝つ

v1 では `PSHADE-2` が線だけ、この単位が塗りだけなので**重ならない**。
`PSHADE-4`（塗りの頂点色）を採るときにこの規則を守ること。

#### 実装の形

- `DrawItem` に**逆 Placement 6 float**と**ランプのストップ範囲**（開始 index と
  本数）を足す。`data1` に空きが 2 つある（`[fill, scaled_stroke, 0, 0]`）ので
  ストップ範囲はそこに入る。逆変換はフィールド追加
- ストップは `path_vertices` と同じ形のストレージバッファで送る
- CPU は `blend_coverage` を「1 色」から「画素ごとにランプを引く」に分岐させる。
  **グラデーションを持たない要素は今の 1 色の経路のまま**（縛り 2）

#### 完了条件

- 線形 / 放射それぞれで、既知の軸に対して既知の画素が期待色になるゴールデン
- **非一様スケール + 回転を掛けた要素**で、グラデーションが図形と一緒に
  変形するゴールデン。**device 空間で射影する実装だと落ちること**を確認する
  （この単位の座標系の判断そのものを pin する）
- 要素ごとに違う軸を `field.apply` で与え、**1 回の rasterize で別々の
  グラデーションが出る**ゴールデン（post-process ではできないことの証明）
- `|end - start| == 0` が先頭ストップの単色になるテスト
- 軸属性が無いとき bbox 既定が使われるテスト
- **グラデーション属性を持たない要素の絵が無改変**であるテスト
- CPU / GPU が許容誤差内で一致する

## やらないこと / 見送る選択肢

- **1 要素内のパターン塗り**。グラデーション以外の塗りは対象外
- **フレーム全体に 1 枚のグラデーション**。`FX-3`（`comp.gradient`）の担当で、
  rasterize を触らずに済む。`PSHADE-6` はその後に、**要素ごとに変えたいときだけ**
  必要になる
- **ランプ属性**（可変長の構造を持つ属性の種類）。`PSHADE-6` は 1 ノード 1 ランプ、
  要素ごとに変わるのは軸、という分割で回避する
- **メッシュプリミティブの描画そのもの**。`rasterize` がメッシュを拒否する
  現在の設計（`ensure_planar_paths`）は、案 C を採ったときだけ問い直す。
  一般的な三角形描画は `3d-scene-plan.md` の `3D-4` の担当
- **可変線幅（テーパー）**。`stroke_width` を Point ドメインで補間する話は
  同じ per-pixel 評価器で書けるが、線の幅が変わると被覆の計算そのものが
  変わる（zeno のストロークが使えなくなる）ので、別の判断が要る。
  `PSHADE-2` が入ったあとに改めて起票する
- **ダッシュの GPU 実装**。弧長はセグメント単位の前計算で足りるので、
  この計画の per-pixel 評価器とは独立。`style-attributes-plan.md` 単位 3 に残す
- **per-pixel 走査を既定経路にすること**。頂点色を持たないパスは今の
  fast path を通り続ける。これを崩すと `RESP3-12` で稼いだ
  CPU ラスタライズ 39.58 → 2.28 ms を失う

## ロードマップ上の位置づけ

フェーズ D「表現力の第一波」の**約束を回収する**位置にある。`STYLE-6`
（#408）までで属性としてのグラデーションは通ったが、画素まで届いていない。

ただし**フェーズ D の他の単位はこれを待たない** — `STYLE-2` / `STYLE-3` /
`VEC-2` / `PARAM-4` / `IMG-2〜6` はどれも独立している。

## 関連文書

- [`style-attributes-plan.md`](style-attributes-plan.md) — 単位 1 / 3 の
  `stroke_align` 繰り延べ、単位 6 の `field.ramp`
- [`properties-parameter-editors-plan.md`](properties-parameter-editors-plan.md)
  — `RampParam` とグラデーションエディタ
- [`roadmap.md`](roadmap.md) — フェーズ D の完成形と、そこに付けた注記
- [`../specifications/procedural-geometry.md`](../specifications/procedural-geometry.md)
  — 標準属性表
