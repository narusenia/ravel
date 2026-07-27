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
| 生成 | Grid / Circle / Line / Box | `shape.*` 5 種 ✅ |
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

- `perimeter` / `area` / `curvature` / `segment_length`。
- 出力ドメインは計測対象に応じて Primitive または Point。

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

### 単位 6: レジストリ / ロケール / 文書

- registry テンプレート、パラメータ範囲、列挙ドロップダウン。
  カテゴリ集計テストの更新。
- `assets/locales/{en,ja}.toml`。
- `docs/specifications/procedural-geometry.md` に標準属性 `piece` と
  本計画のノード一覧を追記。

## 検証

- 全てヘッドレス（`ravel-core` / `ravel-nodes`）。GPU 不要。
- **削除と並べ替えは属性列の取り残しが最も起きやすい**バグなので、
  「全ドメイン × 全属性型」を回すテーブル駆動テストを単位 1・2 に入れる。
- 決定性: `sort(random)` と `blast` は同一入力で 2 回評価して一致を確認。

## 非対象

- **Fuse**（近接点統合）。空間分割構造が要る。
- **Divide / Subdivide / PolyBevel / PolyExtrude**。メッシュ前提なので
  `3d-basics-sketch.md` の押し出し出力形が決まってから。
- **group の作成・合成ノード**。`evaluation-scope-plan.md` の非対象を
  引き継ぐ。当面 `attribute.set` で Bool 列を作る。
- **AttribWrangle 相当**（式で属性を書く）。REQ-CODE-001 待ち。
- **Ray / Intersect**（他ジオメトリへの投影）。空間分割構造が要る。
- **Edit SOP 相当の直接編集**。Viewer 側の仕事で、pen ツール
  （`done/tool-system-plan.md`）が部分的に担っている。
