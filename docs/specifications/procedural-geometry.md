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
4. **2D ファースト**。位置は `Vec2` を基本とし、3D（REQ-MOGRAPH-003）拡張時に
   `Vec3` ドメインを追加する余地を型設計に残す。

## データモデル

### Geometry コンテナ

`ravel-core::geometry`（新モジュール）に定義。

```text
Geometry
├── points:      AttributeSet   (domain = Point)      — P: Vec2 必須
├── primitives:  Vec<Primitive> + AttributeSet (domain = Primitive)
│     Primitive = Path { verts: Range, closed } | …（将来 Mesh）
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

### 標準属性名（予約）

| 名前 | ドメイン | 型 | 意味 |
|------|---------|-----|------|
| `P` | Point/Instance | Vec2 | 位置（必須） |
| `index` | Point/Instance | I32 | 生成順の安定インデックス |
| `id` | Point/Instance | I32 | 寿命を通じ安定な識別子（sim 用） |
| `rot` | Instance | F32 | 回転（rad） |
| `scale` | Instance | Vec2 | スケール |
| `Cd` | Point/Instance | Color | 色 |
| `alpha` | Point/Instance | F32 | 不透明度 |
| `pscale` | Point | F32 | ポイント描画径 |
| `age` / `life` | Point | F32 | パーティクル経過/寿命 |
| `velocity` | Point | Vec2 | 速度（sim） |

### 型変換規約

- Shape 系ノードは FrameBuffer 直描きを廃止し `Geometry` を出力する。
- `Geometry → FrameBuffer` は明示の Rasterize ノードのみが行う
  （パス塗り/ストローク: zeno、ポイント: スプライト描画）。
- 既存 Layer ソース `Shape` はコンパイル時（composition/compile.rs）に
  `ShapeGeometry → Rasterize` チェーンへ展開する。
- `Table`（REQ-DATA-001）は行×型付き列。`Table → Geometry` はバインディング
  ノード（REQ-DATA-002）が行う。

## フィールド

```rust
/// 位置（と任意の入力属性）から値への純関数。バッチ評価が基本。
pub trait Field: Send + Sync {
    fn sample(&self, positions: &[Vec2], ctx: &EvalContext) -> AttributeArray;
}
```

- `Field` はノード間を流れる型（`Arc<dyn Field>` を包む `FieldValue`）。
  遅延評価であり、サンプリングは消費側ノードが行う。
- ビルトイン: ノイズ（simplex/fbm）、フォールオフ（球/線形/パス距離）、
  カーブリマップ、画像サンプラ（FrameBuffer を UV 参照）、Lua 式、
  オーディオ由来スカラー（REQ-MEDIA-003 と接続）。
- 合成: Add / Multiply / Max / Blend ノードで `Field` 同士を結合。
- 消費地点: 属性変調ノード（`attr = field(P)`）、パーティクルフォース、
  per-instance パラメータ変調、統一チャネル値ソース（REQ-CORE-007）。

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
- sim キャッシュは三層キャッシュ（REQ-CORE-006）の RAM 層に載せ、
  ディスク層へのスピルは将来拡張とする。

### スクラブ挙動

| 操作 | 挙動 |
|------|------|
| 後方スクラブ（キャッシュ内） | キャッシュから即表示 |
| 前方再生 | 1 フレームずつ step（通常コスト） |
| 前方ジャンプ | 暫定表示 + バックグラウンドで区間充填 |
| 上流編集 | 影響区間破棄 → 再充填 |

## グラフ内反復（REQ-CORE-013）

v1 では**採用しない**。全要素一括の属性演算 + フィールドで代替し、評価
エンジンは静的 DAG を維持する。要素別分岐はサブグラフ + per-instance 属性で
表現する。MOGRAPH v2 実装完了後に再評価する。

## GPU 方針

- v1 は CPU SoA 評価（rayon 並列、REQ-CORE-005）。
- Rasterize / ポイントスプライトは wgpu 描画（storage buffer へ属性列を
  アップロード）。
- フィールドの WGSL 評価（GPU パーティクル）は REQ-GPU-003 拡張として
  将来対応。`Field` の trait 境界はバッチ評価なので GPU 移行に閉じている。

## 既存コードへの影響

| 箇所 | 改修 | 状況 |
|------|------|------|
| `ravel-core/src/types.rs` | `GeometricData` 実装型の追加 | ✅ `Geometry` が実装（geometry/container.rs） |
| `ravel-core/src/geometry/`（新設） | Geometry / AttributeSet / Field | ✅ 実装済み（画像サンプラ・Lua 式は placeholder） |
| `ravel-core/src/eval.rs` | sim キャッシュ、`StatefulProcessor` 統合 | 🔲 未着手（TASK-041） |
| `ravel-core/src/registry/builtin.rs` | シェイプ系の出力型変更 + 新ノード登録 | 🔶 rasterize テンプレート追加済み、シェイプ系・field 系は未 |
| `ravel-nodes` | シェイプ processor のジオメトリ化、Rasterize、フィールド群 | 🔶 Rasterize（CPU）・field 処理系は実装済み、シェイプジオメトリ化は未（TASK-043） |
| `ravel-core/src/composition/compile.rs` | Shape/Text Layer ソースの展開先変更 |
| `ravel-app`（Node Editor / Properties） | 新型のポート色/接続判定、属性検査 UI |

## 制約・前提

- 属性列の要素数はドメイン内で常に一致（構築時に検証、違反は評価エラー）。
- 文字列属性は低頻度用途（ラベル等）とし、ホットパスでは数値属性を使う。
- `Geometry` の位置は 2D（`Vec2`）。3D 拡張は属性型の追加で行い、
  コンテナ構造は変えない。
- ステートフルノードの多段接続（sim の下流に sim）は v1 では 1 段に制限。
