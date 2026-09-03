# プロシージャルジオメトリ仕様

Houdini / Cavalry / Blender Geometry Nodes 的なプロシージャル自由度を Ravel の
DAG + Hybrid Pull 評価の上に実現するための、ジオメトリ・属性・フィールド・
ステートフル評価のデータモデルと評価規約。

対応要件: REQ-CORE-010 / 011 / 012 / 013、REQ-MOGRAPH-001 / 002 / 004（v2）、
REQ-DATA-001 / 002 / 003。

## 設計原則

1. **属性がすべての中心**。複製・散布・変形・シミュレーションは「要素列 +
   任意名の属性列」への操作として統一する。固定機能のリピーターを作らない。
2. **フィールドは機能横断の変調機構**。パーティクルフォース・per-instance
   変調・属性変形はすべて同一の Field インターフェースを通す。
3. **評価は原則純関数**（time → 値）。状態を持つのはステートフルノードだけで、
   状態は評価エンジン管理の sim キャッシュに閉じ込める。ノード実装が独自に
   内部状態を抱えることを禁じる（イミュータブルグラフ / undo と両立させる）。
4. **2D を既定とし、次元はジオメトリごとに持つ**。位置 `P` の列型は `Vec2`
   （2D）と `Vec3`（3D、REQ-3D-003）のどちらも許す。コンテナ構造は次元で
   分岐させない。2026-07-29 に「位置は `Vec2` を基本とし 3D 拡張時に `Vec3`
   ドメインを追加する」という当初の原則を改めた（下記「位置の次元」）。

## データモデル

### Geometry コンテナ

`ravel-core::geometry`（新モジュール）に定義。

```text
Geometry
├── points:      AttributeSet   (domain = Point)      — P: Vec2 | Vec3 必須
├── primitives:  Vec<Primitive> + AttributeSet (domain = Primitive)
│     Primitive = Path { verts: Range, closed }
│               | Mesh { verts: Range, indices: Range }
├── instances:   AttributeSet   (domain = Instance)   — source: GeometryRef,
│                                                       P / rot / scale / index
└── detail:      AttributeSet   (domain = Detail)     — ジオメトリ全体で1値
```

- 列指向（SoA）。`AttributeArray` は型付き列
  （`F32 | Vec2 | Vec3 | Vec4 | Color | I32 | Bool | Str`）。
- `AttributeSet = HashMap<SmolStr, Arc<AttributeArray>>`。`Arc` により
  構造共有し、変更はコピーオンライト（REQ-CORE-004 の undo モデルと整合）。
- `Geometry` は `NodeData` + `GeometricData`（REQ-CORE-003）を実装し、
  ノード間を `Arc<Geometry>` で流れる。

### インスタンスソース（ジオメトリ / 画像）

instance ドメインがスタンプする対象は `InstanceSource` の 2 種で、
`source_index`（I32）が**画像を含む全ソースの列**を指す。

| 種別 | 中身 | 生成元 |
|------|------|--------|
| `Geometry` | `Arc<Geometry>` | `scatter.*` / `geometry.repeat` ほか |
| `Image` | FrameBuffer（CPU / GPU 常駐のいずれか）+ ピクセル寸法 | `geometry.from_image` |

画像ソースの規約（`done/image-instancing-plan.md` 決定 1 / 5 / 7）:

- **ソースのピクセル寸法がそのままコンポ単位**。矩形は原点中心で、
  アスペクト比は構造上保たれる。
- **コピーごとにネットワークを再評価しない。** 評価は 1 回で、得られた
  FrameBuffer を N 箇所にスタンプする。したがって
  **`scale > 1` のコピーは拡大されてボケる**（標本化はバイリニア）。
  これは既知の仕様であって、副作用でも劣化でもない。
- 矩形の縁は**アンチエイリアスしない**。ピクセル中心が矩形の内側
  （半開区間）なら描き、そうでなければ描かない。
- 重なりは index 順のペインタ方式、tint は `Cd` × `alpha` の乗算で、
  ジオメトリソースと同じ規則に従う。

`rasterize` は画像インスタンスを **CPU 経路でも GPU 経路でも描く**
（Mesh の拒否は従来どおり）。GPU 経路は**サンプルするソースが変わる境目で
描画を分割し**、連続する塊ごとに 1 ドローコールを 1 つのレンダーパスへ積む。
index 順は分割をまたいでも構造的に保たれるので、重なり順は上記の規則の
ままである。

CPU 経路は texel を読むので、GPU 常駐フレームで来た画像はノード入口で
**ソースごとに 1 回だけ**読み戻す（コピー数には比例しない）。**GPU 経路は
読み戻さない** — 常駐フレームはそのままテクスチャとして束ねられ、CPU
フレームだけがアップロードされる。この非対称は意図的な制約である。

### 標準属性名（予約）

| 名前 | ドメイン | 型 | 意味 |
|------|---------|-----|------|
| `P` | Point/Instance | **Vec2 または Vec3** | 位置（必須）。下記「位置の次元」参照 |
| `index` | Point/Instance | I32 | 生成順の安定インデックス |
| `id` | Point/Instance | I32 | 寿命を通じ安定な識別子（sim 用） |
| `rot` | Instance | F32 | 回転（rad）。**2D のみ** |
| `scale` | Instance | Vec2 | スケール。**2D のみ** |
| `orient` | Instance | Vec4 | 姿勢（クォータニオン）。**3D のみ**（REQ-3D-003） |
| `scale3` | Instance | Vec3 | スケール。**3D のみ** |
| `N` | Point/Primitive | Vec3 | 法線。**3D のみ**（ライティングが読む） |
| `Cd` | Point/Instance | Color | 色（＝塗り色。`style.fill` が書く） |
| `alpha` | Point/Instance | F32 | 不透明度 |
| `pscale` | Point | F32 | ポイント描画径 |
| `fill` | Primitive/Instance | Bool | 塗りの有無。`rasterize` の `fill` パラメータが既定（`style.fill` が書く） |
| `stroke_width` | Primitive/Instance | F32 | 線幅（0 = 線なし）。`rasterize` の `stroke_width` パラメータが既定（`style.stroke` が書く） |
| `stroke_color` | Primitive/Instance | Color | 線色。未設定なら `Cd`（＝塗り色）にフォールバック（`style.stroke` が書く） |
| `dash` | Detail | Str | 破線パターン（`"4,2"` 形式。空なら実線。`style.dash` が書く） |
| `dash_offset` | Detail | F32 | 破線の開始位置（`style.dash` が書く） |
| `cap` | Detail | I32 | 端点の形。0=butt / 1=round / 2=square。未設定は round（`style.stroke` が書く） |
| `join` | Detail | I32 | 角の形。0=miter / 1=round / 2=bevel。未設定は round（`style.stroke` が書く） |
| `age` / `life` | Point | F32 | パーティクル経過/寿命 |
| `velocity` | Point | Vec2 | 速度（sim） |
| `u` | Point | F32 | パスパラメータ 0..1。**primitive ごとに正規化**する（`attribute.curveu` が書く。閉パスは閉じる区間の分だけ終点が 1 に届かない） |

### 位置の次元（REQ-3D-003）

`P` の列型は**ジオメトリごとに Vec2 と Vec3 のどちらも許す**。3D 位置を
別属性（`Pw` 等）に分けて `P` を投影後の値にする設計は**採らない** —
位置の情報源が 2 つになり、どちらが新しいかをノードごとに意識する必要が
生じて同期漏れのバグを招く。

投影は `scene.render` の内部で行い、**ジオメトリの `P` を書き換えない**。

**次元はドメインごとに独立**。`Geometry::validate` は Point / Instance
ドメインの `P` が Vec2 か Vec3 であることだけを課す。列は同種なので
ドメイン内の型一致は自動的に保たれ、Point が Vec2 で Instance が Vec3 と
いった組み合わせは許す（複製の実体が 2D、配置が 3D という形が成立する）。

2D 前提のノードが Vec3 の `P` を受けたときの挙動は種別ごとに決める。

| 分類 | 挙動 |
|---|---|
| 変換系（`geometry.transform`） | 成分数で分岐して対応する |
| 境界系（`Geometry::bounds` / `bounds_center`） | 対応する。`bounds` は 2D の `Rect` なので xy 範囲を返す |
| 属性系（`attribute.set` / `.promote` / `.transfer`、`field.apply`） | 次元非依存で素通しする |
| 要素操作系（`geometry.merge`、将来の `blast` / `sort` / `switch`） | 次元非依存 |
| 複製系（`scatter.*`） | 3D 対応は 3D-6。3D の `P` を読むのは `center_input` の再センタリングだけで、そこは**明示エラー** |
| 弧長・パス前提（`attribute.path_sample` / `scatter.path_array`、将来の `resample` / `curveu`） | **明示エラー**。黙って xy に射影しない |
| ラスタライズ（`rasterize`） | **明示エラー**。3D は `scene.render` で描く |

**明示エラーは `GeometryError::RequiresPlanarP`**（`{操作名} requires 2D
positions: `P` is Vec3 …`）。「この操作は 2D の `P` を要求する」ことが
メッセージだけで分かる形にし、上流で `anyhow` へそのまま載せる。

#### `as_vec2` 呼び出しの棚卸し（3D-1a 着手時点: 58 箇所 / 12 ファイル）

`P` を読むのは 58 箇所のうち **35 箇所**（残り 23 箇所は `anchor` / `scale` /
`in_tan` / `out_tan` / `resolution` など**恒久的に Vec2 の列**を読むか、
`typed_accessors!` マクロの定義そのものなので次元の影響を受けない）。
35 箇所の内訳は**製品コード 13 箇所・テスト 20 箇所・例 2 箇所**。

製品コード 13 箇所の分類:

| 箇所 | 分類 | 挙動 |
|---|---|---|
| `geometry/container.rs` `positions_bounds` | 3D 対応 | Vec2 / Vec3 の xy 範囲から `Rect` |
| `geometry/ops.rs` `bounds_center`（2 箇所: point → instance フォールバック） | 3D 対応 | `Vec3` を返す。2D は z = 0 |
| `geometry/ops.rs` `positions`（`attribute.transfer` が使う） | 3D 対応 | 3 成分距離。2D は z = 0 なので算術が一致する |
| 同上（`path_sample` が使う） | 明示エラー | 弧長は 3D で未定義 |
| `nodes/geometry.rs` transform: point `P` | 3D 対応 | Vec3 は 3 成分（スケール → ZYX オイラー → 平行移動） |
| `nodes/geometry.rs` transform: instance `P` | 3D 対応 | 同上。`rot` / `scale` は 2D 専用属性のまま |
| `geometry/field.rs` `apply_field` | 次元非依存 | `P` は書き換えない。フィールドのサンプル位置は xy 射影（3D フィールドは将来拡張） |
| `nodes/rasterize/mod.rs` × 4（GPU flatten / CPU raster、各 point + instance） | 明示エラー | 入口で検証し、インスタンスソースも再帰的に見る |
| `nodes/scatter/mod.rs` `instance_source`（`center_input`） | 明示エラー | `anchor` が Vec2 のみのため 3D の基準点を表現できない |
| `nodes/scatter/mod.rs` `path_array` | 明示エラー | 弧長前提 |

テスト 20 箇所と例 2 箇所は**既存の 2D ケースをそのまま検証し続ける**ので
変更しない（`P` が Vec2 のままなら `as_vec2` は今までどおり通る）。3D の
挙動は新しいテストで追加する。

### プリミティブ種別（REQ-3D-003）

`Primitive` は `Path`（折れ線）と `Mesh`（三角形メッシュ）の 2 種別。
**種別と `P` の次元は独立した軸**で、Vec2 の `P` を持つ平面の三角形分割も、
Vec3 の `P` を持つ 3D 折れ線も、どちらも成立する。

```text
Primitive = Path { verts: Range, closed }
          | Mesh { verts: Range, indices: Range }
```

`Mesh` の `indices` は `Geometry` が 1 本だけ持つ**共有インデックス列**
（`Vec<u32>`）への範囲で、3 個ずつ 1 三角形として読む。`AttributeSet` の
列と同じく `Arc` で構造共有し、点だけを編集したコピーがインデックス列を
複製しないようにする（REQ-CORE-004）。

**インデックスの値は `verts.start` からの相対オフセット**とする。絶対の点
インデックスにすると `geometry.merge` が連結のたびに全三角形を書き換える
ことになるが、相対なら `verts` と `indices` の 2 つの範囲をずらすだけで
済み、インデックス列は追記のみで動く。`Geometry::validate` は
`indices` 範囲が共有列に収まること・3 の倍数であること・各値が
`verts.len()` 未満であることを検査する。

Path 前提のノードが `Mesh` を受けたときの挙動は種別ごとに決める。

| 分類 | 挙動 |
|---|---|
| 構造検査（`Geometry::validate`） | 対応する。`verts` の範囲検査に加え、インデックス列の範囲・3 の倍数・頂点数未満を検査 |
| 要素操作系（`geometry.merge`、将来の `blast` / `sort` / `switch`） | 種別非依存で素通しする。`Primitive::shifted` が両 variant を同じ 2 オフセットで再配置する |
| 属性系（`attribute.set` / `.promote` / `.transfer`、`field.apply`） | 種別非依存で素通しする。列だけを触りトポロジを見ない |
| 弧長・パス前提（`attribute.path_sample` / `scatter.path_array`、将来の `resample` / `curveu`） | **明示エラー**。Mesh に弧長は定義されない。黙って読み飛ばさない |
| ラスタライズ（`rasterize`） | **明示エラー**。三角形は `scene.render` が描く（3D-4） |

**明示エラーは `GeometryError::RequiresPathPrimitives`**（`{操作名}
requires path primitives: …`）。`RequiresPlanarP` と同じ粒度で、「この操作は
Path を要求する」ことがメッセージだけで分かる形にする。

**読み飛ばしを禁じる理由**: 弧長・ラスタライズ系のループはプリミティブを
走査して `Path` にだけ反応するので、Mesh を単に無視すると mesh のみの
ジオメトリは空の結果（空フレーム / 0 コピー）になり、混在ジオメトリは
面だけが黙って消える。どちらも原因の手がかりが残らないため、入口で拒否する。

#### `Primitive::Path` 参照の棚卸し（3D-1b 着手時点: 53 箇所 / 7 ファイル）

53 箇所のうち**挙動の判断が要るのは 8 箇所**（残り 45 箇所は Path を
構築するだけの箇所 6・文書コメント 3・既存テスト 36 で、種別が増えても
判断が生じない）。

挙動の判断が要る 8 箇所の分類:

| 箇所 | 分類 | 挙動 |
|---|---|---|
| `geometry/container.rs` `validate` | 対応 | `verts` 検査に加えインデックス列を検査 |
| `nodes/geometry.rs` `merge`（destructure + 再構築の 2 箇所） | 種別非依存 | `Primitive::shifted` で両 variant を再配置し、インデックス列を連結 |
| `nodes/rasterize/mod.rs` `path_vertex_mask` | 種別非依存 | `Primitive::verts()` で参照点を覆う |
| `geometry/ops.rs` `path_sample` | 明示エラー | 弧長は Mesh で未定義 |
| `nodes/rasterize/mod.rs` × 2（GPU flatten / CPU raster の走査） | 明示エラー | 入口の `ensure_planar_paths` で検証し、インスタンスソースも再帰的に見る |
| `nodes/scatter/mod.rs` `collect_path_segments` | 明示エラー | 弧長前提。`path_array` の入口で検証 |

構築 6 箇所・テスト 36 箇所は**既存の 2D ケースをそのまま検証し続ける**ので
変更しない（Path を作る側は種別が増えても影響を受けない）。Mesh の挙動は
新しいテストで追加する。

### 回転の表現（REQ-3D-003）

**オーサリングと要素で分ける。**

| 用途 | 表現 | 理由 |
|---|---|---|
| 人がキーフレームを打つ回転（Scene オブジェクト、レイヤー殻、ノードパラメータ） | オイラー角の成分別チャンネル（`Channel3`） | 統一アニメーションチャネル（REQ-CORE-007）は**成分ごとに独立補間する**ため、クォータニオンをキーフレーム対象にできない（成分別補間は回転にならない） |
| 要素ごとの回転（Instance / Point 属性） | クォータニオン（`orient`: Vec4） | ノードが計算する値でチャンネルを通らない。slerp や look-at をノード内で正しく書ける |

**回転順は ZYX（外因性 = 固定軸まわり、Z → Y → X の順に適用）に固定する。**
列ベクタへの行列積では `Rx * Ry * Rz`。等価な内因性の順序は X → Y' → Z''。
実装とテストで pin し、後から変えない（既存プロジェクトの姿勢が変わるため）。

変換は内部で行列に畳んで適用するが、**行列を属性として持たない**
（16 float / 要素になり、position / rotation / scale を別々に変調できなくなる）。

回転の数学（オイラー ⇄ クォータニオン、合成、slerp、3x3 行列）は
`geometry::rotation` が単独で持つ。各ノードで三角関数を書き下ろさない。
クォータニオンの成分順は `(x, y, z, w)` で、`orient` 列の要素と一致させる。

**Y が ±90°（ジンバルロック）の姿勢からオイラー角へ戻すときは、X を 0 とし、
結合した回転量を Z に載せる。** この縮退では行列から復元できるのは X と Z の
和（+90°）か差（−90°）だけで、分配の仕方を選ぶ必要がある。復元した 3 値を
再度行列に畳めば同じ姿勢になる（変わるのはオイラー角の組であって姿勢ではない）。

### Primitive の種別と 2D/3D 命名規約

`Primitive` は `Path` と `Mesh` の 2 種別を持つ。**各ノードは種別を網羅して
扱い、未対応の種別で panic しない**（明示エラーか素通しかをノードごとに宣言し、
この仕様書に記載する）。

2D と 3D で**アルゴリズムが本質的に分岐する**ノードだけ variant を作る。
位置の次元で分岐すれば済むものは 1 ノードで両方を扱う。

| | ラベル | type_key |
|---|---|---|
| 次元非依存 | `Transform` | `geometry.transform` |
| 2D 専用（3D 兄弟あり） | `Cell Fracture` | `geometry.cell_fracture` |
| 3D 版 | `Cell Fracture 3D` | `geometry.cell_fracture_3d` |

- **3D だけ `_3d` を付ける。** 2D は素の id にする。既存プロジェクトの
  type_key が変わらないので、3D 版を後から足してもマイグレーションが不要
- **ラベルも 3D だけ明示する。** 既定（2D / 次元非依存）に印を付けない。
  `3D` 兄弟の存在そのものが素の側を 2D 専用だと示す
- variant を作る例: `cell_fracture` / `path.boolean` 対 `mesh.boolean` /
  `shape.*` 対プリミティブ生成
- variant を作らない例: `geometry.transform`、`field.apply`、`attribute.*`、
  `geometry.blast` / `sort`、`scatter.*`

### 型変換規約

- Shape 系ノードは FrameBuffer 直描きを廃止し `Geometry` を出力する。
- `Geometry → FrameBuffer` は明示の Rasterize ノードのみが行う
  （パス塗り/ストローク: zeno、ポイント: スプライト描画）。
- **`Scene → FrameBuffer` は `scene.render` のみが行う**（REQ-3D-001）。
  Mesh を含むジオメトリは `rasterize` ではなく Scene 経由で描く。
  `Geometry → Scene` は `scene.add` が 3D 変換と組にして行う。
- 既存 Layer ソース `Shape` はコンパイル時（composition/compile.rs）に
  `ShapeGeometry → Rasterize` チェーンへ展開する。
- `Table`（REQ-DATA-001）は行×型付き列。`Table → Geometry` はバインディング
  ノード（REQ-DATA-002）が行う。

## フィールド

```rust
/// 位置（と任意の入力属性）から値への純関数。バッチ評価が基本。
pub trait Field: Send + Sync {
    fn sample(&self, input: &FieldSample<'_>) -> AttributeArray;

    /// 自身とラップしているフィールドを含むおおよそのバイト数。
    /// `FieldValue` の `NodeData::byte_size` を通じてキャッシュ予算に載る。
    /// **既定実装は置かない**（`0` は黙った過少計上になる）。
    fn byte_size(&self) -> u64;
}

/// フィールドが読める入力。追加してもシグネチャを壊さないよう構造体で渡す。
pub struct FieldSample<'a> {
    pub positions: &'a [Vec2],
    /// サンプル対象ドメインの全属性。`index` / `id` / 任意のユーザ列を
    /// 駆動値にできる。
    pub attributes: &'a AttributeSet,
    pub ctx: &'a EvalContext,
}
```

- `Field` はノード間を流れる型（`Arc<dyn Field>` を包む `FieldValue`）。
  遅延評価であり、サンプリングは消費側ノードが行う。
- ビルトイン: ノイズ（simplex/fbm）、フォールオフ（球/線形/パス距離）、
  属性読み出し（`field.attribute`。`index` 等を駆動値にする。列の
  `[min, max]` を `[0, 1]` へ写す `normalize` 付き）、
  カーブリマップ、カラーランプ（`field.ramp`。スカラーを `in_min` /
  `in_max` で正規化しストップ列で色を引く。**数値から色を作れる唯一の
  フィールド**で、スカラーフィールドではグレースケールにしかならない
  `Cd` / `stroke_color` の変調に色相を与える）、
  画像サンプラ（FrameBuffer を UV 参照、未実装）、
  式（`field.expression`。要素ごとに評価する REQ-CORE-015 で、言語は
  REQ-CORE-014 と共通の専用スカラー式言語。**Lua ではない** —
  Lua は REQ-CODE-001 のコード Layer 専用）、
  時刻（`field.time`。`frame` / `seconds` / `normalized` を `scale` /
  `offset` 付きで返す。変調をアニメーションさせる駆動源）、
  定数（`field.constant`。`multiply` と組んで減算・除算を表す）、
  オーディオ由来スカラー（REQ-MEDIA-003 と接続）。
- 合成: Add / Multiply / Max / Blend ノードで `Field` 同士を結合。
- 消費地点: 属性変調ノード（`attr = field(P)`）、パーティクルフォース、
  per-instance パラメータ変調、統一チャネル値ソース（REQ-CORE-007）。

### 変調の書き戻し（`field.apply`）

サンプルした値を既存の属性列へ書き戻す規約。`apply_field` と
`FieldApply` が実装する。

**合成モード**は `Set` / `Add` / `Multiply` / `Min` / `Max` の 5 種。
`amount` は合成結果への補間率として全モード共通に作用する。

```text
result = existing + (combine(existing, sampled) - existing) * amount
```

- 既定は `Set`。この式に入れると `existing + (sampled - existing) * amount`
  となり、単純なブレンドと項まで一致する。独立した `Blend` モードは
  持たない（`Set` + `amount` と同じものになる）。
- `amount` は `0..=1` にクランプする。**`amount = 0` は入力列をそのまま
  返す**（ビット等価）。ゼロ補間でも合成演算を先に評価すると `-0.0` が
  `+0.0` に化け、中間値が溢れた場合は `inf * 0 = NaN` になるため。

**成分マスク**（`components`）は、スカラーフィールドを多成分属性へ適用する
ときに書き換える成分を選ぶ。成分は位置で解決するので `x`/`r`、`y`/`g`、
`z`/`b`、`w`/`a` は同じ枠を指し、`"xy"` / `"rgb"` / `"a"` のように並べる。

- **未指定は「対象型が決める」**。Color / Vec4 は `rgb`（アルファを保つ）、
  それ以外は全成分。スカラー値は選択された全成分へブロードキャストされる
  ので、既定でアルファも書くと「暗くすると同時に透明になる」。
  アルファを動かすには `a`（または `rgba`）を明示する。
- 対象型に無い成分だけを指定した場合（`Vec2` に `"z"`）は黙って no-op に
  せず、警告して全成分へフォールバックする（group 名と同じ規約）。
- 逆方向（ベクタフィールド → スカラー属性）は無い。スカラー以外の
  フィールドは既存列との**型完全一致**を要求する。

**変調できる型**は `F32` / `Vec2` / `Vec3` / `Vec4` / `Color`。
`I32` / `Bool` / `Str` は成分を持たないので
`FieldError::UnsupportedAttributeType` で拒否する。判定は `amount` の
評価より前に行うので、`amount = 0` でも同じエラーになる。

対象列が無い場合は既定で作る（`create_if_missing`）。`stroke_color` /
`stroke_width` のように誰かが変調するまで存在しない属性を、前段に
`attribute.set` を挟まずに変調できるようにするため。

どの要素に作用するかは group が決める（下記「要素スコープ」）。`amount` は
soft な重み付け、`group` は hard な適用可否で直交する。

## ステートフル評価（sim キャッシュ）

### 問題

Hybrid Pull（REQ-CORE-002）は「フレーム t の値は t だけから決まる」前提。
パーティクル等は前フレーム状態に依存するため、そのままでは表現できない。

### 規約

```rust
pub trait StatefulProcessor {
    type State: Send + Sync;              // Arc で保持されるフレーム状態
    fn initial(&self, ctx: &EvalContext, inputs: &Inputs) -> Self::State;
    fn step(&self, prev: &Self::State, ctx: &EvalContext, inputs: &Inputs)
        -> Self::State;                    // 純関数: (state_{t-1}, t) → state_t
}
```

- 評価エンジンはステートフルノードごとに **sim キャッシュ**
  `Vec<Arc<State>>`（フレーム連続区間）を保持する。
- フレーム t の Pull 要求に対し、未計算区間 `[last+1, t]` を順に `step` して
  埋める。区間評価は評価スレッドプールで行い UI を塞がない
  （REQ-CORE-005）。長距離ジャンプ時は最後のキャッシュ済み状態を暫定表示。
- **無効化**: 上流サブグラフの構造/パラメータハッシュを sim キャッシュに
  記録し、変化したら全区間破棄（v1）。パラメータのキーフレーム変化は
  影響開始フレーム以降のみ破棄（v2 最適化）。
- **決定性**: 乱数は `seed` パラメータ + `id` 属性由来のハッシュのみ。
  `step` が同一入力で同一出力を返すことをテストで担保する。
- sim キャッシュは汎用の評価キャッシュとは別の map で保持する（区間が
  順序を持ち、逐次充填という別のアクセスパターンだから）。バイト予算は
  REQ-CORE-006 の単一予算の中に**保護枠**として確保し、通常のフレーム
  キャッシュの圧力では削られない。ディスク層へのスピルは将来拡張とし、
  長期保持は明示キャッシュノードに寄せる。

### スクラブ挙動

| 操作 | 挙動 |
|------|------|
| 後方スクラブ（キャッシュ内） | キャッシュから即表示 |
| 前方再生 | 1 フレームずつ step（通常コスト） |
| 前方ジャンプ | 暫定表示 + バックグラウンドで区間充填 |
| 上流編集 | 影響区間破棄 → 再充填 |

## 評価スコープ軸（REQ-CORE-002 / REQ-CORE-011 / REQ-CORE-013）

キャッシュ・dirty のキーは `NodeKey { path: Vec<PathSegment>, node }`。
`path` は「**同じノードを別スコープで評価する**」ための軸で、レイヤー
ネットワークと subnet のネストがここに乗る（REQ-LAYER-003/007）。

同一ノードの複数の評価結果を同時に保持する必要がある機能は、独自の
キャッシュを持たず**この軸を拡張する**。

| バリアント | 用途 |
|---|---|
| `Layer(comp, layer)` | レイヤーの所有ネットワーク |
| `Subnet(node)` | subnet の内部グラフ |
| `Comp(comp)` | ネストコンポジション（PreComp、v2 予約） |
| `Iteration(node, i)` | グラフ内反復の i 回目 |
| `TimeShift(node, frame)` | タイムリマップ等の別フレーム評価 |

例外はシミュレーションキャッシュ（REQ-CORE-011）のみ。フレーム連続区間は
順序に意味を持つ系列で逐次充填という別のアクセスパターンを持つため、
専用の `SimTrack` を維持する。ただしキー型は `NodeKey` を共有する。

## グラフ内反復（REQ-CORE-013）

**ピース単位**で採用する（2026-07-27 に v1 不採用を撤回、理由は
REQ-CORE-013 を参照）。整数属性（既定 `piece`）の値ごとにジオメトリを
分割し、値の種類数だけ内部ネットワークを評価する。`path` に
`Iteration(node, i)` を積むだけなので、評価エンジンは静的 DAG のまま。

- **要素単位の反復は採用しない**。全要素同時変調は属性 + フィールドが担う。
- 反復のネストは不可（1 段）。
- 反復回数は上限を持ち、超過は評価エラー（黙って切り捨てない）。

## 要素スコープ（group）

group 専用の型は導入しない。**Bool 属性を group として扱う**
（Houdini 自身の内部表現と同じ）。

ジオメトリドメインの op は `group` 文字列パラメータを取る。

- 空文字列 = 全要素（既定）
- 属性名 = その Bool 属性が true の要素のみに作用
- 対象外の要素は入力の値をそのまま通す（削除しない）
- 存在しない名前・Bool でない属性は全要素にフォールバックして警告
- **列そのものが無いときは通す値も無い**ので、属性を書く op は「未設定と
  同じ意味の値」を group 外へ置く（`style.fill` なら `fill` に `rasterize`
  のパラメータ既定）。`attribute_set_in_group` の `unset` 引数がそれ

フィールドの `amount` は soft な重み付け、`group` は hard な適用可否で、
両者は直交する。両方指定した場合は「group 内の要素にのみ amount を適用」。

## GPU 方針

- v1 は CPU SoA 評価（rayon 並列、REQ-CORE-005）。
- Rasterize / ポイントスプライトは wgpu の instanced-quad render pass で実装済み。
  flatten した属性とパス頂点を storage buffer へアップロードし、fragment shader
  で non-zero winding と edge distance を評価するため、凹形状・自己交差も
  triangulation なしで扱う。通常ノードは RGBA32Float の `GpuFrameBuffer` を返し、
  Composition synthetic / Viewer ad-hoc は golden 互換の CPU zeno 経路を使う。
- 画像インスタンスは**同じ instanced-quad パスの 3 種目の draw item**として
  描く。fragment で配置行列を逆変換して UV を求め、CPU 参照と同じ
  プレマルチプライ加重のバイリニアで標本化する。テクスチャは
  **ソースが変わる境目で描画を分割**して束ね直す方式で（テクスチャ配列は
  ソースの解像度が揃うことを要求し、アトラスは詰め替えのコストと上限を
  負うため）、順序は分割の構造として保たれる。パスと点だけの run は
  読まないプレースホルダ 1 px を束ねる。
- フィールドの WGSL 評価（GPU パーティクル）は REQ-GPU-003 拡張として
  将来対応。`Field` の trait 境界はバッチ評価なので GPU 移行に閉じている。
- **Mesh は既存のラスタライザで描けない。** 現在の `rasterize.wgsl` は
  フラグメントごとにパスセグメントへの最短距離を評価する解析的方式で、
  頂点バッファを使わず `depth_stencil: None`。Mesh の描画は
  **第 2 のレンダーパイプライン**（頂点/インデックスバッファ、深度添付、
  法線補間）になる。`Primitive::Mesh` の enum 追加とレンダラの実装は
  別単位として扱う（`3d-scene-plan.md`）。
- 描画順は不透明 Mesh が深度バッファ、半透明と Path が**オブジェクト単位の
  代表深度**（変換とカメラ行列から求めた view 空間の重心 z）でのソート、
  という 2 パス（REQ-3D-007）。**奥行き専用の属性は持たない** — 位置は `P`
  だけで、奥行きは変換とカメラから決まる。**交差する半透明同士は保証しない** —
  ソートがオブジェクト単位なのでオブジェクト内部の深度差を解決できず、
  かつ解析的カバレッジはアルファなので半透明部分が深度を書くと縁で背景が抜ける。

## 既存コードへの影響

| 箇所 | 改修 | 状況 |
|------|------|------|
| `ravel-core/src/types.rs` | `GeometricData` 実装型の追加 | ✅ `Geometry` が実装（geometry/container.rs） |
| `ravel-core/src/geometry/`（新設） | Geometry / AttributeSet / Field | ✅ 実装済み（式フィールドは REQ-CORE-015 の式言語で実装済み、画像サンプラは未） |
| `ravel-core/src/eval.rs` | sim キャッシュ、`StatefulProcessor` 統合 | 🔲 未着手（TASK-041） |
| `ravel-core/src/registry/builtin.rs` | シェイプ系の出力型変更 + 新ノード登録 | ✅ shape / scatter / attribute / field / rasterize テンプレート登録済み |
| `ravel-nodes` | シェイプ processor のジオメトリ化、Rasterize、属性・フィールド群 | ✅ shape / scatter / attribute / field と rasterize の GPU + CPU reference processor 実装済み |
| `ravel-core/src/composition/compile.rs` | Shape/Text Layer ソースの展開先変更 | 🔶 Shape は `ShapeGeometry → Rasterize` 展開済み、Text は未 |
| `ravel-app`（Node Editor / Properties） | 新型のポート色/接続判定、属性検査 UI | 🔶 GEOMETRY/FIELD ポート色・Viewer 表示は実装済み、属性検査 UI（TASK-047）は未 |

## 制約・前提

- 属性列の要素数はドメイン内で常に一致（構築時に検証、違反は評価エラー）。
- 文字列属性は低頻度用途（ラベル等）とし、ホットパスでは数値属性を使う。
- `Geometry` の位置は `P` の列型で表し、Vec2（2D）と Vec3（3D）の**どちらも
  許す**。コンテナ構造は変えない（`Primitive` の種別追加は構造変更に含めない）。
  2026-07-29 に「位置は 2D。3D 拡張は属性型の追加で行う」という当初の決定を
  改めた — 3D 位置を別属性にすると位置の情報源が 2 つになるため（REQ-3D-003）。
- ステートフルノードの多段接続（sim の下流に sim）は v1 では 1 段に制限。
