# ジオメトリ操作ノード拡充 実装計画

> **Status**: Planned — 2026-07-27

対象: プロシージャルなジオメトリ操作の語彙を、Houdini の中核 SOP に相当する
範囲まで埋める。関連要件: REQ-CORE-010、REQ-CORE-012、REQ-MOGRAPH-001。

**前提**: `evaluation-scope-plan.md`（group 規約と反復）。本計画のノードは
その規約の上に載る。

## 問題

現在のジオメトリ操作は「作る」と「変える」に偏っていて、
**「減らす」「並べ替える」「測る」が丸ごと無い**。

| 分類 | Houdini の中核 SOP | Ravel の現状 |
|---|---|---|
| 生成 | Grid / Circle / Line / Box | `shape.*` 7 種（rect / ellipse / polygon / star / line / grid / custom_path）✅（単位 11。`scatter.grid` は点を配るだけなので `shape.grid` とは別物） |
| 複製 | Copy to Points | `scatter.*` 4 種 ✅ |
| 変形 | Transform | `geometry.transform` ✅ |
| 結合 | Merge | `geometry.merge` ✅ |
| 属性 | AttribCreate / Promote / Transfer | `attribute.*` 4 種 ✅ |
| **削除** | **Blast / Delete** | **無し** |
| **並べ替え** | **Sort** | **無し** |
| **再分割** | **Resample / Divide** | **無し** |
| **統合** | **Fuse** | **無し** |
| **計測** | **Measure** | **無し** |
| **分岐** | **Switch** | **無し** |
| 整理 | Null | 無し |
| **反復複製** | **Copy Stamp / Transform 累積** | **無し**（`scatter` は位置を配るだけ） |
| **デフォーマ** | **Bend / Twist / Taper** | **無し** |
| **group 生成** | **Group / Group Expression** | **無し**（Bool 列を作る手段が全要素ブロードキャストのみ） |
| **整列・分布** | — | **無し** |

### group と反復が半端になる

`evaluation-scope-plan.md` は group 規約（Bool 属性で要素を絞る）と
ピース単位反復を入れる。だが **group で絞れても消せない**。

Houdini で group が効くのは Blast とセットだから。「この group を消す」
「この group だけ残す」ができないと、group はパラメータのフィルタにしか
ならず、構造を作る道具にならない。反復も同じで、ピースごとに処理しても
不要なピースを落とせない。

**`attribute.delete`（`per-instance-modulation-plan.md` MOD-4）は
属性列の削除であって、要素の削除ではない。** 全くの別物。

### stagger の質が `index` の生成順に縛られる

`index` は生成順に振られる固定値。`scatter.grid` なら行優先の順序に
なる。したがって index 駆動の stagger は「行優先で順に」しかできない。

モーショングラフィックスで実際に欲しいのは「左から」「中心から外へ」
「ランダムに」「パス沿いに」。Houdini の Sort SOP に相当するものが要る。

`per-instance-modulation-plan.md` が stagger を主要ユースケースに
挙げているのに、**その表現力の上限を `index` の生成順が決めてしまう**。

## 決定事項

### 削除は Blast 1 ノードに統一する

Houdini は Blast（group を消す/残す）と Delete（式で消す）に分かれるが、
Ravel は group 規約が Bool 属性なので 1 つで足りる。

`geometry.blast`: `group` / `domain` / `invert`。
`invert` で「消す」と「残す」を切り替える。

削除に伴う整合は明示的に扱う。ポイントを消せばそれを参照する
プリミティブが壊れるので、**プリミティブは参照点が 1 つでも消えたら
消える**（Houdini と同じ規則）。`index` は詰め直し、`id` は保存する。

### Sort は「並べ替え」であって「値の書き換え」

`geometry.sort` は要素の**格納順**を並べ替え、`index` を振り直す。
`id` は保存する。属性列は全て同じ置換で並べ替える。

モード: `x` / `y` / `radial`（指定中心からの距離）/ `along_path`
（パス入力への射影距離）/ `random`（seed）/ `attribute`（指定属性の値）/
`reverse`。

これで stagger が「左から」「中心から」「ランダムに」になる。
**`per-instance-modulation-plan.md` の stagger はこのノードとセットで
初めて実用になる。**

### Resample はパス専用

`geometry.resample`: パスプリミティブ上に等間隔（または指定分割数）で
点を打ち直す。`length` / `segments` / `keep_corners`。

タイポグラフィのパス沿い配置（TYPE-4）とパーティクルのエミッタ
（PART-2）が両方これを要求する。既存の `path_sample`（1 点だけ取る）
とは別物。

### Measure は属性を書くノード

`geometry.measure`: `perimeter` / `area` / `curvature` /
`segment_length` を指定ドメインの属性として書く。

フィールドの駆動源として効く（`field.attribute` で読める）。
「長い辺だけ太くする」のような変調がこれで書ける。

### Switch と Null は安いので同じ計画に入れる

`geometry.switch`: 可変入力から `index` パラメータで 1 つ選ぶ。
`geometry.null`: 恒等。グラフの整理とリンク先の固定用。

いずれも数十行だが、無いとグラフが組みにくい。

### Fuse は本計画に入れない

近接点の統合は空間分割構造（グリッドハッシュ）が要り、他のノードと
実装コストの桁が違う。パス編集の後始末が主用途で、`shape.custom_path` と
pen ツールが現状の主な生成源であることを踏まえると優先度は低い。

## 実装単位

各単位は独立。並列委譲しやすいよう分けてある。

### 単位 1: `geometry.blast`（要素削除）

- `group` / `domain` / `invert`。
- ポイント削除時、参照点を失ったプリミティブも削除。
- `index` の詰め直し、`id` の保存。
- インスタンスドメインの削除では `instance_sources` の参照も整理。

**完了条件**

- group 内が消え、group 外が**属性値ごと不変**であるテスト。
- `invert` で補集合が消えるテスト。
- ポイント削除でプリミティブの `verts` 範囲が正しく詰まるテスト。
- 全要素削除で空ジオメトリになり `validate()` が通るテスト。
- 削除後に `index` が 0..n-1 に詰まり `id` が保存されるテスト。

### 単位 2: `geometry.sort`（並べ替え）

- 上記 7 モード。`index` 振り直し、`id` 保存。
- 全属性列に同じ置換を適用。
- `random` は seed 決定的（`scatter.scatter` と同じハッシュ規約）。

**完了条件**

- 各モードで期待順になるテスト。
- 属性列が**すべて**同じ置換で並ぶテスト（1 列でも取り残すとデータが
  ずれるので網羅する）。
- 同一 seed で `random` が再現するテスト。
- プリミティブの `verts` 参照がポイント並べ替え後も正しいテスト。
- `field.attribute(index) → apply` と組んで stagger の順序が変わる
  結合テスト。

**実装時の決定（`Primitive::Path` が連続範囲であることの帰結）**:
`geometry.connect`（単位 12）と同じ制約に当たる。`Primitive::Path` は
`verts: Range<usize>` で連続した点の並びしか張れないため、ポイントドメインの
置換がプリミティブを跨ぐと `verts` はそのままで形が変わる。よって
`geometry.sort` は次の形にした。

- **ポイントドメインの置換は各プリミティブの頂点範囲の内側に閉じる**
  （範囲の外の点はまとめて 1 つの範囲として扱う）。プリミティブが無い点群は
  全体が 1 範囲なので自由に並ぶ ＝ stagger が要求する経路はそのまま通る。
  複数パスを持つジオメトリは各パスの頂点だけが並び替わる
- `Primitive::Mesh` は三角形インデックスが `verts.start` 相対なので、
  ポイントドメインの並べ替えは明示エラー（`RequiresPathPrimitives`）。
  プリミティブドメインの並べ替えは `Primitive` 値ごと動くので mesh も安全
- プリミティブドメインは `P` を持たないので、位置モードの基準は
  **そのプリミティブの点の重心**。単位 4 の `measure` が bounds を書けば
  そちらからも参照できる
- 属性モードのキーは F32 / I32 / Bool / Str はその値、ベクタと色は
  **第 1 成分**（Houdini の Sort と同じ既定）
- **降順パラメータは持たない。** `reverse` モードでもう 1 回並べれば済む
- `random` のハッシュは `scatter.*` と同一の実装を共有する
  （`geometry::ops::element_hash` に移し、`scatter` が import する）
- ノードのアイコンは `assets/icons/arrow-down-up.svg`（Lucide v0.462.0）

### 単位 3: `geometry.resample`

- `length` / `segments` / `keep_corners`。
- 既存の弧長計算（`geometry/ops.rs` の `path_sample`）を共有する。
- 属性は元の点から線形補間。

**完了条件**

- 直線パスで等間隔になるテスト。
- 閉パスの周回が正しいテスト。
- `keep_corners` で角が保存されるテスト。
- 属性の補間テスト。
- 退化パス（点が 1 つ、長さ 0）でエラーにならないテスト。

### 単位 4: `geometry.measure`

- `perimeter` / `area` / `curvature` / `segment_length` / `bounds` / `size`。
- 出力ドメインは計測対象に応じて Primitive / Point / Detail。
- `bounds`（Detail、Vec4）と `size`（要素ごと、Vec2）は単位 9 が使う。

**完了条件**

- 既知の形状（矩形・円）で解析解と一致するテスト。
- 自己交差パスでの面積の定義（符号付き）を明示したテスト。
- 開パスの `area` の扱い（閉じたとみなす）のテスト。

### 単位 5: `geometry.switch` / `geometry.null`

- `switch`: 可変入力 + `index`。範囲外はクランプ。
- `null`: 恒等（入力の `Arc` をそのまま返す）。

**完了条件**

- `switch` の選択と範囲外クランプのテスト。
- `null` が入力と `Arc::ptr_eq` になるテスト（コピーしない）。

### 単位 6: `geometry.group_index`（index による要素指定）

group 規約（`evaluation-scope-plan.md`）は Bool 属性を group として扱うが、
**その Bool 列を作る手段が `attribute.set`（全要素ブロードキャスト）しか
無い**。「5 文字目だけ」「偶数番だけ」を指定できない。

- `range`（`"3"` / `"3-7"` / `"3,5,9"` / `"0-20:2"` のストライド記法）
- `domain` / `name`（出力する Bool 属性名）/ `invert`
- 範囲外の指定は警告して無視（エラーにしない）

`geometry.transform`（単位 7 で group 対応）と組めば
「文字 5 だけ 30° 回す」が書ける。

**完了条件**

- 各記法のパーステスト。
- 範囲外指定が警告 + 無視になるテスト。
- 生成した group で `transform` が対象要素のみに効く結合テスト。

### 単位 7: `geometry.repeat`（トランスフォームリピータ）

`scatter.*` は位置を配るだけで**変換が累積しない**。螺旋・入れ子・
フラクタル的な反復は現状表現できない。

- `count` / 1 コピーあたりの `translate` / `rotate` / `scale`。
- コピー i の変換は「1 コピー分の変換を i 回合成」（累積）。
- 出力はインスタンスドメイン。`index` / `P` / `rot` / `scale` を付与し、
  `scatter.*` と同じ形にする（下流が区別しなくてよい）。
- 元ジオメトリを `instance_source` に置く。

**完了条件**

- `count = 1` で元と同じ位置に 1 つだけ出るテスト。
- 変換が累積することのテスト（i 番目 = 変換^i）。
- スケール累積で 0 に潰れる場合の扱いのテスト。
- 螺旋配置のゴールデンテスト。

### 単位 8: デフォーマ（`geometry.bend` / `twist` / `taper`）

ノイズで `P` を歪めることはできるが、**制御された変形**が無い。

- 共通: 軸（`axis`）、範囲（`start` / `end`）、`amount`、`group`。
- `bend`: 軸に沿って円弧状に曲げる。
- `twist`: 軸周りに位置に比例して回す。
- `taper`: 軸に沿って幅を変える。
- いずれも**点位置の純関数**として実装し、接線（`in_tan` / `out_tan`）も
  同じ変換で移す（曲線が壊れないように）。

**完了条件**

- `amount = 0` で入力と一致するテスト。
- 各デフォーマの既知形状での値検証。
- **接線が追随することのテスト**（これを落とすと曲線が折れる）。
- `group` 指定で対象外要素が不変であるテスト。

### 単位 9: `geometry.distribute`（要素サイズを考慮した分布）

素朴な整列・等間隔は既存の変調で半分書ける（`field.constant → apply(P, set, x)` /
`field.attribute(index, normalize) → apply(P, set, x)`）。ノードにする
価値があるのは**それだけでは届かない部分**。

- **バウンディングボックス基準**の整列（左端 / 中央 / 右端）。
  単位 4 の `measure` が bounds を Detail 属性に書けば、
  フィールド側からも参照できるようになる。
- **エッジ間隔での分布** — 要素ごとの**サイズ**が要る。中心間の等間隔と
  隙間の等間隔は別物で、後者は変調では書けない。
- インスタンスソースのサイズを考慮した配置。

`measure` に `bounds` / `size` の出力を追加する（単位 4 の拡張）。

**完了条件**

- 中心間等間隔と隙間等間隔が異なる結果になるテスト。
- 幅の異なる要素での隙間等間隔の値検証。
- バウンディングボックス基準の整列 6 種のテスト。
- 要素 1 個 / 2 個での退化ケース。

### 単位 11: `shape.line` / `shape.grid`

問題の表にある「生成」の欠落分。`shape.custom_path` はペンツール専用で
メニューから追加できない（`node_editor.rs` の `CUSTOM_PATH_TYPE_KEY` フィルタ）
ため、**2 点を結ぶ線をノードで作る手段が無い**。

- `shape.line`: 始点 / 終点（Vec2）、分割数。開パスを 1 本出す。
  分割数 > 1 のとき中間点を等間隔に置く（`field.apply` の変調対象になる）
- `shape.grid`: 行数 / 列数 / サイズ。格子状の**パス**（行と列の線）を出す。
  点だけが欲しい場合は `scatter.grid` を使う、という住み分けを文書化する

**完了条件**

- `shape.line`: 分割数 1 で 2 点、n で n+1 点になるテスト。
- `shape.line`: 始点 = 終点（退化）でエラーにならないテスト。
- `shape.grid`: 行数 × 列数 に対する primitive 数が定義どおりであるテスト。
- 両ノードが `ParamRole`（`done/viewer-overlay-manipulator-plan.md` 単位 5）を
  宣言していることのテスト。
  **この条件は `OVL-5` が `ParamRole` を入れるまで満たせない。** 型そのものが
  まだ存在せず（`registry/builtin.rs` の doc コメントに名前が出るだけ）、
  `OVL-5` は未着手。**回収は `OVL-5` 側で行う**ので、この単位は残りの条件で
  完了とし、宣言とそのテストは `OVL-5` の完了条件に持たせる。

### 単位 12: `geometry.connect`（要素を結ぶ）

Houdini の Add SOP に相当する。点群を線で結ぶ手段が無いため、
`scatter.*` で配った点をワイヤーフレームとして見せられない。

- 結び方: `order`（`index` 順）/ `nearest`（近傍 k 個）/ `group`（同一
  group 内のみ）
- 補間: `linear` / `bezier`。`bezier` は結ぶ点の `in_tan` / `out_tan`
  （`geometry/names.rs:36-40`）を書き、隣接点の方向から接線を推定する
- 閉じるかどうかのフラグ
- **点は増やさない**。primitive（パス）を追加するだけ。属性は元の点のまま

**完了条件**

- `order` で index 順に 1 本のパスができるテスト。
- `nearest` の決定性テスト（同一入力で 2 回評価して一致）。
- `bezier` で `in_tan` / `out_tan` が書かれ、`rasterize` が曲線として
  描くテスト。
- 点が 1 つ以下のときエラーにならないテスト。
- 結んだ後も元の点属性（`Cd` / `pscale` 等）が保存されるテスト。

**実装時の決定（`Primitive::Path` が連続範囲であることの帰結）**:
`Primitive::Path` は `verts: Range<usize>` で**連続した点の並び**しか張れない
（`geometry/container.rs`）ため、任意の 2 点を結ぶ辺の集合は点を複製しないと
表現できない。「点を増やさない」を守るため、`geometry.connect` は次の形にした。

- 3 モードとも**パスは 1 本**で、点を**並べ替える**（増やさない・減らさない）。
  `order` は恒等置換、`nearest` は先頭からの貪欲近傍チェーン（k 個の辺を張る
  グラフではない）、`group` はメンバーを先頭に集める
- 属性列は同じ置換で並べ替えるので、値は点に付いたまま。**`index` は振り直さない**
- 入力のプリミティブは**置き換える**（Houdini Add SOP の
  「Delete Geometry But Keep The Points」と同じ扱い）。Mesh 入力は明示エラー
- 近傍探索は `geometry/ops.rs` の `PointGrid`（`MED-CORE-05`）を使う。
  訪問済みが k 個の候補を埋めたら線形走査に落ちるので、長い鎖の末尾は O(n²)

### 単位 13: `attribute.curveu`（パスパラメータ）

パスに沿った変調ができない原因。標準属性にパスパラメータが無く
（`geometry/names.rs` は P / anchor / index / rot / scale / Cd / alpha /
pscale / age / life / velocity / in_tan / out_tan のみ）、それを書くノードも
無い。`attribute.path_sample` は `distance` を 1 つ受けて**単一点**を返すだけ
（`crates/ravel-nodes/src/attribute/mod.rs:139-146`）。

- `geometry/names.rs` に `U` を予約（F32、Point ドメイン。Houdini の
  `curveu` 相当）
- `attribute.curveu`: 各点に primitive 内の弧長比 0..1 を書く。
  `by_arc_length`（既定）と `by_vertex_order` を切り替えられる
- 弧長計算は単位 3（`geometry.resample`）と単位 4（`geometry.measure`）と
  同じ `geometry/ops.rs` の実装を共有する
- 複数 primitive があるとき、`u` は**primitive ごとに** 0..1 で正規化する
  （ジオメトリ全体で通し番号にしない）

これで「線に沿ったグラデーション」が既存ノードだけで繋がる:

```text
shape.line → attribute.curveu → field.attribute("u") → field.ramp
  → field.apply(target = "Cd") → rasterize
```

`field.attribute` は任意の属性列を読める（`geometry/field.rs:307`）ので既存で
足り、`field.ramp` は `style-attributes-plan.md` が追加する。

**完了条件**

- 直線パスで `u` が等間隔になるテスト。
- 不均等な点間隔のパスで `by_arc_length` と `by_vertex_order` が異なる値を
  返すテスト。
- 複数 primitive でそれぞれ 0..1 に正規化されるテスト。
- 閉パスで終点の `u` が定義どおり（1 か 0 か）であることのテスト。
- 上記の「線に沿ったグラデーション」経路が通ることの結合テスト
  （`field.ramp` 到着後に有効化）。

### 単位 10: レジストリ / ロケール / 文書

**単位 1〜9 と 11〜13 のすべてを対象にする**（最後に実施する）。

- registry テンプレート、パラメータ範囲、列挙ドロップダウン。
  カテゴリ集計テストの更新。
- `assets/locales/{en,ja}.toml`。
- `docs/specifications/procedural-geometry.md` に標準属性 `piece` と `u`、
  本計画のノード一覧を追記。
- 本計画冒頭の「生成」行の現状を、単位 11 の実装後に更新する。

## 検証

- 全てヘッドレス（`ravel-core` / `ravel-nodes`）。GPU 不要。
- **削除と並べ替えは属性列の取り残しが最も起きやすい**バグなので、
  「全ドメイン × 全属性型」を回すテーブル駆動テストを単位 1・2 に入れる。
- 決定性: `sort(random)` と `blast` は同一入力で 2 回評価して一致を確認。

## 非対象

- **Fuse**（近接点統合）。空間分割構造が要る。
- **`shape.box`**（矩形の枠線）。`shape.rect` + `style.stroke`
  （`style-attributes-plan.md`）で足りるため、単位 11 には含めない。
- **Divide / Subdivide / PolyBevel / PolyExtrude**。メッシュ前提なので
  `3d-scene-plan.md` の `Primitive::Mesh` が入ってから（表現は決着済みで、
  メッシュ操作としての設計が残っている）。
- **group の合成ノード**（AND / OR / NOT）。単位 6 は index からの生成のみ。
- **レイヤーの整列**。`align-panel-plan.md`（レイヤー Transform の x/y を
  書き換える UI）。単位 9 はジオメトリ内の要素が対象。
- **AttribWrangle 相当**（式で属性を書く）。REQ-CODE-001 待ち。
- **Ray / Intersect**（他ジオメトリへの投影）。空間分割構造が要る。
- **Edit SOP 相当の直接編集**。Viewer 側の仕事で、pen ツール
  （`done/tool-system-plan.md`）が部分的に担っている。
