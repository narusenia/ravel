# Ravel — データモデル仕様書

## 概要

Ravelのデータモデルは3層で構成される: (1) ノードグラフ（DAG）の構造定義、(2) ノード間を流れるデータ型、(3) プロジェクトファイルの永続化形式。

## ノードグラフモデル

### ノード (Node)

```rust
struct Node {
    id: NodeId,
    type_key: NodeTypeKey,        // "blur", "color_correct", "sequence" 等
    parameters: Vec<Parameter>,
    inputs: Vec<InputPort>,       // InputPort.is_param: 公開パラメータポート
                                  // （Graph::expose_param_port、末尾 append。
                                  //  evaluator が process 前に入力を分離し
                                  //  型変換して ResolvedParams へ上書き。
                                  //  優先順位: attribute > pin > parameter）
    outputs: Vec<OutputPort>,
    position: Vec2,               // エディタ上の位置
    metadata: NodeMetadata,
    // サブネットノードのみ Some（REQ-LAYER-003）。ノードが内部 Graph を
    // 所有する（Layer::network と同型の所有構造、REQ-LAYER-009）。
    // Arc 共有によりノード複製は安価で、内部編集は replace_node で
    // ノードごと差し替える（イミュータブル維持）。
    subnet: Option<Arc<Graph>>,
}

struct NodeMetadata {
    label: Option<String>,        // ユーザー定義ラベル
    color: Option<Color>,         // エディタ上のノード色
    collapsed: bool,
    bypassed: bool,               // Bypass: 評価器が process を呼ばず、
                                  // 出力ポートの型に一致する最初の入力値を
                                  // そのまま出力する（eval.rs 参照）
}
```

`Graph` 自体も serde 対応（ノード/エッジを ID 昇順の `Vec` に射影する
決定的形式。読み込みは `Graph::from_parts` を通り再検証される）。
サブネットの入れ子 Graph はこの形式で `Node.subnet` ごと永続化される。

### エッジ (Edge)

```rust
struct Edge {
    id: EdgeId,
    source: (NodeId, OutputPortIndex),
    target: (NodeId, InputPortIndex),
}
```

### パラメータ (Parameter)

```rust
struct Parameter {
    key: String,
    value: ParameterValue,
}

enum ParameterValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    // アニメーション可能な値（統一チャネル、REQ-LAYER-004）
    Channel(AnimationChannel),              // スカラー（f32 相当）
    Channel2([AnimationChannel; 2]),        // Vec2
    Channel3([AnimationChannel; 3]),        // Vec3 / RGB
    Channel4([AnimationChannel; 4]),        // Vec4 / RGBA
    // ペンツールのパス制御点（REQ-UI-011）。定数のみ（パスアニメーションは
    // 将来の PathChannel 設計）。接線は p からのオフセット、ゼロ = コーナー
    PathPoints(Vec<PathPoint>),             // PathPoint { p, in_tan, out_tan }
    // スカラー変換カーブ（`field.curve_remap` の制御点）。定数のみ
    // （カーブ自体のアニメーションは v1 非対象）。v6 より前は
    // `"0:0,1:1"` 文字列だった
    Curve(CurveParam),                      // CurvePoint { x, y, interpolation, tangents }
}
```

`PathPoints` と `Curve` は**必ず末尾に足す**。bincode は variant を位置で
索引するので、途中に挿入すると既存 journal が読めなくなる。追加自体は
`JOURNAL_FORMAT_VERSION` の更新で覆う。

- ネットワーク内の**任意のノードパラメータ**がチャネルを持てる（キーフレーム、
  ノード出力バインド、ブレンド。Expression / AudioReactive は placeholder）。
- Int / Bool は v1 では定数のみ（step キーは v2）。PathPoints も定数のみ
  （journal v6 で追加。`in_tan`/`out_tan` 点属性として Geometry に展開
  され、曲線区間は rasterize が共有フラット化で消費する）。
- `Curve` も定数のみ（journal v7 で追加）。`CurveParam` は `KeyframeCurve` と
  補間種別・接線規約・区間規約を共有し、入力軸だけが整数フレームでなく
  任意スカラー。定義域外は両端値にクランプ、制御点が空なら恒等
  （`evaluate(x) == x`）。制御点は**入力昇順・一意・有限**が不変条件で、
  デシリアライズもこれを維持する（非有限を落とし、並べ替え、重複入力は
  後の点を残す）。
- プロセッサは構築時にパラメータをキャプチャ**しない**。Evaluator が各
  `process()` 呼び出し時にフレーム解決した `ResolvedParams` を渡す
  （アニメーション中のプロセッサ再構築を防ぐ）。
- `NodeTemplate`（registry）の `ParamRange` は UI 用の範囲・デフォルト情報
  として残る。

### サブネットワーク (Subnet, REQ-LAYER-003)

> **v3 変更**: 旧 `Subgraph` 構想（別レジストリの `SubgraphId` 参照）は
> 廃止。入れ子グループは **`Node.subnet: Option<Arc<Graph>>`**（ノードが
> 内部 Graph を直接所有する形）で実装された。

- type_key は `subnet`。内部に `net.in` / `net.out` を 1 つずつ持つ。
- 内部 In のカスタム出力ポート = サブネットの入力ピン、内部 Out の
  入力ポート = 出力ピン（型制約なし・複数可。多出力は `PortRecord`）。
- **未接続の入力ピンは、サブネットノード自身の同名パラメータから解決**
  される（Houdini の promote 相当）。パラメータも無ければ内部 In の
  デフォルトに落ちる。優先順: 接続値 > サブネットのパラメータ >
  内部 In のパラメータ。
- 評価は `EvalScope::evaluate_sub(PathSegment::Subnet(node_id), …)` の
  再帰（レイヤー境界と同一機構）。キャッシュ/dirty は所有パス
  （`CompId / LayerId / [SubnetNodeId ...] / NodeId`）単位。入れ子深さに
  制限なし。
- 親グラフでは入出力ポートを持つ 1 ノードとして表示される。
  ダブルクリックで中に潜る UI は Phase 3。

### Composition / Layer モデル（レイヤーネットワークモデル, v3）

> **v2 からの変更**: 「Layer = LayerSource + ビルトイン Transform + エフェクト
> サブグラフ」および「Composition 全体の平坦化コンパイル（Evaluator 変更不要）」
> を撤回。**1 レイヤー = 殻 + 1 ノードネットワーク**（Houdini 的）に移行。
> 詳細要件は REQ-LAYER、実装計画は
> `docs/implementation/done/layer-network-model-plan.md` を参照。

#### Composition

```rust
struct Composition {
    id: CompId,
    name: String,
    resolution: (u32, u32),
    frame_rate: FrameRate,
    duration_frames: u64,
    layers: im::Vector<Layer>,     // 下から上への合成順序
    background_color: Color,
}
```

Composition はドキュメント層に `im::HashMap<CompId, Arc<Composition>>` として保持し、
Graph と同様にイミュータブル操作 + 構造共有で undo 対応。

#### Layer（殻 + ネットワーク）

```rust
struct AudioSource {
    asset_id: String,              // media_assets のキー（映像と同じアセット表）
    stream_index: usize,           // コンテナ内の音声ストリーム番号
    gain: AnimationChannel,        // レイヤーローカルフレームで評価
    fade_in_frames: u64,
    fade_out_frames: u64,
    audio_muted: bool,             // Layer.muted と独立した音声のみの mute
}

struct Layer {
    id: LayerId,
    name: String,
    network: Graph,                // 所有するノードネットワーク（REQ-LAYER-009）
    // 時間配置（AEセマンティクス: start=配置, in/out=トリム）
    start_frame: i64,              // Comp タイムライン上の開始位置（負も可）
    in_frame: u64,                 // ソース内の表示開始フレーム
    out_frame: u64,                // ソース内の表示終了フレーム [in, out)
    audio: Option<AudioSource>,     // 音声を持つ場合の殻プロパティ
    // ビルトイン Transform（殻の first-class プロパティ）
    transform: LayerTransform,     // anchor_point/position/scale/rotation
    opacity: AnimationChannel,
    // 合成
    blend_mode: BlendMode,
    adjustment: bool,              // 調整レイヤー（REQ-LAYER-010）
    // 状態
    solo: bool,
    muted: bool,
    locked: bool,
    // 親子
    parent: Option<LayerId>,       // Transform 継承（P/R/S のみ、opacity/blend は継承しない）
    // v2 予約フィールド（評価されない。永続化互換のため存在）
    time_remap: Option<AnimationChannel>,
    track_matte: Option<TrackMatte>,
}
```

- **`LayerSource` enum は廃止**。レイヤー「種類」（Solid / Video / Shape /
  Audio / Text / PreComp / Null）は作成時テンプレートに降格し、初期ネットワークを
  生成するだけ（REQ-LAYER-008）。データモデル上、全レイヤーは同一構造。
- **テンプレートはデータ駆動**（`composition::templates`）。定義は
  `LayerTemplate`（ノード列 + シンボリックキーのエッジ列、RON
  シリアライズ可能）で、ビルトインの Solid / Shape / Video / Audio / Null は
  `assets/layer-templates/*.ron` を埋め込み提供
  （`builtin_layer_templates()`）。インスタンス化は NodeRegistry の
  型定義（ポート・デフォルトパラメータ）をシードにテンプレート側が
  上書き・追加し、`NodeId::next` で毎回新 ID を採番する。Text / PreComp
  テンプレートは対応ノード実装後に追加（v2）。
- **Null レイヤー**は「ネットワークの Out に `frame` ポートが無いレイヤー」
  として再定義。マージチェーンに参加せず、Layer Ref 経由でのみ消費される
  （REQ-LAYER-005）。
- **Audio レイヤー**も In / Out だけで `frame` 出力を持たないため、映像の
  マージチェーンには参加しない。時間配置は `Layer` の `start_frame` /
  `in_frame` / `out_frame` を共有し、`gain` は他の殻チャネルと同じく
  レイヤーローカルフレームで評価する。
- **調整レイヤー**（`adjustment = true`）は、In の `source` ポートに下位
  スタックの合成結果を受け取り、その出力が次の background になる
  （`background' = network(background)`。opacity はエフェクト強度）。

#### ネットワークインターフェース（In / Out ノード, REQ-LAYER-002）

全レイヤーネットワークは `net.in` / `net.out` を1つずつ持つ（型キーで識別）。

- **`net.in`**（殻 → ネットワークの注入点）: 固定出力 `base_geometry`
  （GEOMETRY、レイヤー幅×高さの quad）と `t`（SCALAR、レイヤーローカル時間・
  秒）と `f`（SCALAR、レイヤーローカルフレーム番号）、調整レイヤーでは
  `source`（FRAME_BUFFER）、さらにユーザー定義の
  カスタムパラメータポート（Float / Int / Bool / Vec2 / Vec3 / Color）。
  `f` ポートを持たない既存ドキュメントはロード時に末尾へ追補される
  （インデックス参照のエッジは不変）。
  カスタムパラメータは殻の Properties パネルに自動露出しキーフレーム可能。
- **`net.out`**: 入力 `frame`（FRAME_BUFFER、殻が消費する唯一のポート）+
  カスタム出力ポート（任意型。Layer Ref から参照される）。
- 多出力ノードの評価値は `PortRecord`（出力ポート順の値ベクタ）で、
  エッジの `source_port` でインデックスされる。

#### 所有権と ID（REQ-LAYER-009）

ネットワークはオーナーが所有する入れ子構造（Layer → Graph、将来の
サブネットノード → 内部 Graph）。ノード ID は**ドキュメント内でグローバル
一意**とする（`NodeId::next` 採番。永続化は読み込み時にこの不変条件を
維持する）。プロセッサのレジストリはこの不変条件の下で NodeId のみで
索引される。評価キャッシュ・dirty 集合は**所有パス**
（`CompId / LayerId / [SubnetNodeId ...] / NodeId`）をキーとする。
所有パスは ID 衝突のためではなく、同一グラフが複数のオーナー（将来の
共有サブネット・PreComp インスタンス）経由で評価される際の、
評価インスタンス区別のために使う。

#### 殻のコンパイル（REQ-LAYER-007）

殻の合成チェーン（時間変換 → Transform → Opacity → Merge）は synthetic
ノードとして従来通りコンパイルするが、レイヤーネットワークは**平坦化
しない**。旧 `Source → TimeOffset → Effects` の位置には**ネットワーク境界
ノード**（`comp.network`）が1つ入るだけで、境界ノードがレイヤーの
ネットワークを再帰的に pull 評価する。

```
normal layer:     [Network boundary] → Transform → Opacity → Merge
adjustment layer: [Network boundary] → Transform → Merge(adjustment)
                       ▲ source（下位スタック）  ▲ background
```

**決定論的 ID**: 殻の synthetic ノードの ID は `(CompId, LayerId, Role)`
から決定論的に導出（`comp_id << 32 | layer_id << 8 | role`、Role =
Network/Transform/Opacity/Merge）。再コンパイルで ID が安定し、Evaluator
のキャッシュが維持される。Synthetic ノードは `metadata.synthetic = true`
で、永続化除外・ノードエディタ非表示の規約。

**殻プロセッサの意味論**（Phase 2 で実装済み、CPU リファレンス実装）:

- `comp.transform`: レイヤーの Transform チャネル
  （anchor / position / scale / rotation。**rotation は度**）を評価し、
  親チェーン（P/R/S 継承）を合成した 2D アフィンを逆写像 +
  premultiplied バイリニア補間で適用する。チャネルは**レイヤーローカル
  フレーム**で評価し、レイヤー値は process 時に Document から読む
  （構築時キャプチャ禁止の不変条件）。恒等変換はパススルー。
  行列そのものは `ravel_core::composition::transform`
  （`Affine` / `layer_matrix` / `world_matrix`）が持ち、Viewer の bbox・
  ヒットテスト・パスオーバーレイも**同じ関数**を使う（描画とオーバーレイが
  ずれないための単一供給源）。`world_matrix` は親を可視性に関係なく辿り、
  各親を**その親自身のローカルフレーム**で評価する。
- `comp.opacity`: レイヤー opacity（ローカルフレーム評価、0–1 clamp）を
  アルファに乗算。opacity = 1 はパススルー。
- `comp.merge.*`: straight-alpha の Porter-Duff over。ブレンドモード
  （add / multiply / screen / overlay）は W3C 合成モデル
  （`(1-ab)·Cf + ab·B(Cb,Cf)` を over に通す）で、背景が透明なら
  どのモードもフォアグラウンドに一致する。
- `comp.merge.adjustment`: `mix(background, adjusted, opacity)`
  （premultiplied 空間で補間）。opacity はエフェクト強度
  （REQ-LAYER-010）。表示区間外は background バイパス。

**レイヤーローカル時間**（REQ-LAYER-006）: 境界ノードは EvalContext を
ローカル時間（`comp_frame - start_frame + in_frame`、秒ベース）に書き換えて
内部評価に渡す（スコープ付き EvalContext）。表示区間 `[in, out)` の外では
ネットワークを評価せず透明フレームを返す。タイムリマップは v2
（`time_remap` 予約フィールド）。

**Layer Ref**（`layer.ref`、REQ-LAYER-005）: パラメータは `layer`
（同一コンポ内の参照先 LayerId、Int）と `port`（参照する `net.out`
ポート名、既定 `frame`）。所有パスの最内 `PathSegment::Layer` から
「同じコンポジション」を解決し、参照先ネットワークの **pre-transform の
素の出力**を、参照先の殻の時間配置を適用したローカル時刻で評価して返す。
参照先の表示区間外は型付きゼロ（透明フレーム / 空 Geometry / 0）。
solo / mute はマージチェーンのみに作用し、Layer Ref の解決には影響しない。
循環は `composition::validate::validate_layer_ref_cycles`（サブネット
内部も走査）が編集/コンパイル時に拒否し、評価器のスコープ再入ガードが
実行時にも遮断する。

**メディアアセット**（REQ-LAYER-008 / REQ-PROJ-001）: `Document.media_assets:
im::HashMap<String, MediaAssetEntry>` が評価時のアセット表
（`ravel-core::composition::asset`）。
`media` ノードは `asset_id` パラメータでこの表を引き、`AssetKind` に応じて
3 経路でフレームを得る。`Container` はレイヤーローカル時間（秒）から
`media_frame = floor(t · media_fps)`（ストリーム末尾に clamp）でフレームを
要求する — 異 fps メディアは秒ベースで整合する（REQ-LAYER-006）。
`Still` は 1 枚をデコードしてプロセッサ内に Arc キャッシュする。
`Sequence` は `start + floor(t · seq_fps)` を `start..=end` に clamp した
番号のフレームファイルを組み立てて読む（seq_fps は
`metadata.frame_rate`、未設定ならコンプの fps）。コンテナのデコードは
`MediaReader` 抽象経由、静止画・連番は単一画像リーダ経由で、FFmpeg 実装は
`ravel-nodes` の `ffmpeg` feature で有効化。オフライン
（`resolved == None`）またはデコード失敗時は評価を失敗させず、ctx の
解像度の透明フレームを返す（警告はアセットごとに 1 回）。
旧 `type_key: "video"` はロード時に
`Document::normalize_node_type_aliases` が `media` へ書き換える
（永続互換の alias）。アセット参照の管理（相対化・解決）はアプリ層の
責務で、評価は `resolved` だけを読む（下記「アセット参照モデル」）。

**Rasterize の色決定**（REQ-LAYER-008）: 要素色の優先順は
`Cd`/`alpha` 属性 > `color` 入力ピン > `color` パラメータ（既定は
不透明白）。属性欠落時のみピン/パラメータが丸ごと代替し、インスタンスの
tint は乗算のため中立（白）フォールバックを保つ。

**Evaluator の変更（v3 で受け入れ）**: Document-aware（境界ノード・
Layer Ref が他レイヤーのネットワークを解決）、スコープ付き再帰評価
（`EvalScope::evaluate_sub`）、評価時パラメータ解決（`ResolvedParams`）。

#### 設計上の注意事項（Fable レビュー指摘）

- **alpha 規約**: FrameBuffer は straight（非 premultiplied）alpha で
  受け渡す（merge.wgsl / rasterize の実装規約）。補間・混合が必要な箇所
  （transform のバイリニア、adjustment の mix）は内部で premultiply して
  計算し straight に戻す。
- **solo の扱い**: solo は Comp 全体に影響（any solo → 非 solo を非表示）。展開前のプレパスで処理。
- **PreComp 循環検出**: PreComp ノード（`precomp`、v2）の `comp_id` 参照を
  レイヤーネットワーク走査で検出・拒否（`composition/validate.rs`）。
  Layer Ref の循環も同層で検出する（REQ-LAYER-005）。
- **fps/解像度不一致**: 子 Comp / 異 fps メディアは秒ベースでマッピング（REQ-LAYER-006）。
- **フレーム範囲**: `[in, out)` 半開区間。
- **time remap**: v2 対応。`time_remap: Option<AnimationChannel>` 予約済み。
- **muted Layer と Parenting**: 親子付けは可視性と独立（REQ-LAYER-001）。
  muted / 非-solo の親は synthetic ノードごとコンパイルされない（プレパスで
  除外される）が、子の `comp.transform` が `world_matrix` で Document から
  親チェーンを辿るので、親の変換は子に効き続ける。この経路はグラフのエッジに
  現れない Document 側依存なので、殻編集の無効化は親だけでなく**子孫の
  synthetic ノードのキャッシュも落とす**（`Evaluator::set_document`）。
  コンパイル済み `parent_transform` エッジは active な親にしか張れない
  （非 active な親にはソースノードが無い）依存エッジで、値は読まれない。
- **negative start_frame**: Layer の start_frame は i64（負も可）。Comp 先頭より前に配置可能。

## データ型ヒエラルキー

```
NodeData (trait)
├── BufferData (trait)
│   ├── FrameBuffer          # RGBA f32 ピクセルバッファ
│   ├── DepthBuffer          # 単チャネル f32
│   └── MultiLayerBuffer     # マルチレイヤーEXR
├── TemporalData (trait)
│   ├── Clip                 # フレーム列 + メタデータ
│   └── TimeRemap            # タイムリマップカーブ
├── GeometricData (trait)
│   ├── Shape                # 2Dパスデータ
│   ├── Mask                 # マスクデータ
│   ├── Mesh3D               # 3Dメッシュ（基本機能用）
│   └── ParticleSystem       # パーティクル群
├── NumericData (trait)
│   ├── Scalar(f32)
│   ├── Vec2(f32, f32)
│   ├── Vec3(f32, f32, f32)
│   ├── Vec4(f32, f32, f32, f32)
│   ├── Color(f32, f32, f32, f32)
│   └── Curve(KeyframeCurve)
├── AudioData (trait)
│   ├── AudioBuffer          # PCM f32 バッファ
│   └── SpectrumData         # FFT解析結果
└── TextData (trait)
    ├── PlainText(String)
    └── RichText             # スタイル情報付き
```

## アセット参照モデル

`ravel-core::composition::asset`。メディアは `.ravprj` に埋め込まず**参照だけ**を
持つ（REQ-PROJ-001）。

```rust
struct MediaAssetEntry {
    path: AssetPath,              // 永続。相対 / 絶対 / 変数
    kind: AssetKind,              // 永続
    metadata: AssetMetadata,      // 永続。probe で埋まる
    #[serde(skip)]
    resolved: Option<PathBuf>,    // 実行時のみ。app が注入。None = オフライン
}

enum AssetPath {
    Absolute(PathBuf),            // "/Users/me/footage/clip.mov"
    Relative(String),             // "./footage/clip.mov"（プロジェクトルート基準）
    Variable(String),             // "${PROJECT_ROOT}/footage/clip.mov"
}

enum AssetKind {
    Container,                    // FFmpeg で開けるコンテナ（映像 + 任意の音声）
    Still,                        // 単一画像
    Sequence { prefix: String, suffix: String, padding: usize, start: u64, end: u64 },
}

struct AssetMetadata {
    width: Option<u32>,
    height: Option<u32>,
    frame_rate: Option<FrameRate>,
    duration_secs: Option<f64>,
    codec: Option<String>,
    color_space: Option<String>,
    audio_stream_count: usize,
    audio_streams: Vec<AudioStreamMetadata>,   // 追加フィールド（format v4 同居）
    file_size: u64,
}

struct AudioStreamMetadata {
    stream_index: usize,          // **コンテナ内**のストリーム番号
    codec: Option<String>,
    sample_rate: u32,
    channels: u32,
}
```

`audio_streams` は `AudioSource::stream_index` に入れるべき番号
（= デコーダが seek する**コンテナのストリーム番号**）を持つ。映像 0 /
音声 1 のクリップなら `stream_index: 1`。format v4 が出た後に
`#[serde(default)]` で追加したフィールドなので、それ以前に書かれた文書では
空で、`audio_stream_count` だけが本数を記録している（本数からコンテナの
ストリーム番号は復元できないため、`first_audio_stream_index()` は `None`）。

`AssetPath` は**単一の文字列**として永続化する（`${` を含めば `Variable`、
絶対なら `Absolute`、それ以外は `Relative`）。RON が読みやすくなるうえ、
format v3 の `MediaAssetEntry { path: PathBuf }`（常に絶対）がそのまま
`Absolute` として読めるため v3 → v4 の文書側マイグレーションが不要になる。
絶対判定は POSIX とドライブレター / UNC の両方を見る — プロジェクトは
プラットフォームをまたいで移動する。

**責務の分離**:

- 永続化されるのは `path` / `kind` / `metadata`。`resolved` は保存しない。
- **保存時**、`resolved`（絶対パス）を基準に `path` を書き直す:
  プロジェクトルート配下なら `Relative`、それ以外は `Absolute`。
  ユーザーが明示設定した `Variable` と、オフライン（`resolved == None`）の
  エントリは書き換えない。書き換えは保存するスナップショットにだけ効き、
  メモリ上の `Document` は汚さない（保存が編集扱いにならない）。
- **読み込み時**、`path` をプロジェクトルート（`.ravprj` を置くディレクトリ）と
  変数表で解決して `resolved` を埋める。解決できなければ `None` = オフライン。
  オフラインのアセットを指す `media` ノードは評価を失敗させず、ctx の解像度の
  透明フレームを返す（`docs/implementation/media-import-plan.md` の決定 7）。
- **`Save As` でプロジェクトルートが変わったとき**、メモリ上の `resolved` は
  更新しない。`Absolute` / `Relative` は `resolved` が既に絶対なので影響しないが、
  `Variable`（`${PROJECT_ROOT}/…`）だけは旧ルートを指したままになる。
  変数パスを設定する UI はまだ無い（単位 6）ので現状は到達不能。
  **単位 6 で変数パス編集を入れるときに `Save As` 後の再解決を実装すること**。
- したがって「保存 → プロジェクトディレクトリごと移動 → 再オープン」で
  参照は解決したままになり、「保存 → ロード → 保存」はバイト一致する。

プロキシ（`ProxyInfo`）とハッシュによる同一性判定は未実装。将来
`MediaAssetEntry` の予約フィールドとして再導入する。

## 公開パラメータ宣言モデル (REQ-PROJ-006)

プロジェクトの**外部契約**。CLI のテンプレートレンダリング（REQ-RENDER-005）、
サブグラフテンプレートの公開入力（REQ-PLUGIN-005 (2)）、シェーダ manifest
（REQ-GPU-003 / REQ-PLUGIN-002）が**同じ 1 つの機構**に乗る
（`ravel-core::exposed`）。

```rust
struct ExposedParameter {
    name: String,            // 契約名。一意・トリム済み
    value_type: ExposedType, // float / int / bool / string /
                             // vec2 / vec3 / vec4 / color / media
    default: ExposedValue,   // value_type と一致し、有限
    description: String,     // 呼び出し側に見せる説明（省略可）
    binding: ExposedBinding, // { node: NodeId, key: String }
}

struct ExposedParameters { entries: Vec<ExposedParameter> }  // 順序＝提示順
```

- **契約は名前であってパスではない。** 束縛は `NodeId` + パラメータキーで、
  ノード ID は文書全体で一意（REQ-LAYER-009）かつ改名・再配線で変わらない。
  レイヤー名やノードパスは外部契約に出さないので、レイヤーを改名しても
  呼び出し側の `--param` は壊れない。
- **不変条件は 3 つ**: 名前の一意性、既定値が宣言型と一致すること、既定値が
  有限であること。コンストラクタと `Deserialize` の両方が強制する。
  `.ravprj` は手編集・マージされるテキストなので、不変条件を破る宣言は
  **その宣言だけを捨てて**プロジェクトは開く（警告に出す）。
- **追従が要るのはパラメータキーの改名だけ。** ポート改名は「4 箇所を 1 操作で
  書き換える」（`network-interface-editing-plan.md`）に宣言の束縛を 5 番目として
  加え、`KeyRename` が同一 Document スナップショットで運ばれる。
- **値の範囲は定数と素材参照に限る。** キーフレーム・式・`PathPoints` ・
  `Curve` は非対象で、宣言は値ソースを置き換えず `ChannelSource::Constant` の
  値だけを書き換える。定数でない成分は据え置き、`BindingIssueReason` として
  報告する（`ravel-core::exposed::apply`）。
- **束縛先が消えても宣言は残る。** 解決不能として `apply::resolve` が報告し、
  機械可読な列挙（`ExposedListing`）は `resolved: false` を付ける。隠すと
  「そんな名前は無い」と「その名前の裏側が壊れている」が区別できない。

### サブグラフテンプレート (REQ-PLUGIN-005 (2))

サブネットの内部グラフ + そのサブネット内に束縛された宣言を 1 ファイルに
まとめたもの（`ravel-core::subgraph_template::SubgraphTemplate`）。

```rust
struct SubgraphTemplate {
    name: String,
    inner: Graph,                    // net.in / net.out を含む内部グラフ
    declarations: ExposedParameters, // 上と同じ型・同じ検証
}
```

- **宣言の型を 2 系統に分岐させない。** テンプレートの公開入力は
  `ExposedParameters` そのもので、読み込み時に通る検証も `.ravprj` と同一。
- **インスタンス化は ID を振り直す**（`Graph::duplicate_with_fresh_ids`。
  入れ子サブネットとノード出力束縛も追従）。宣言の束縛も同じ対応表で書き換わる
  ので、同じテンプレートを何度貼っても互いを踏まない。
- 生成された宣言は `Document.exposed_parameters` に**加える**ので、
  `.ravprj` のフォーマットは変わらない。名前が衝突したときは
  `subgraph_template::add_declarations` が `<name>_2` / `_3` へ付番し、
  行った改名を呼び出し側へ返す。

## 永続化形式

### manifest.json

```json
{
  "format_version": 7,
  "ravel_version": "0.1.0",
  "project_name": "My Lyric Video",
  "created_at": "2026-06-22T10:00:00Z",
  "modified_at": "2026-06-22T15:30:00Z",
  "frame_rate": { "num": 30, "den": 1 },
  "resolution": { "width": 1920, "height": 1080 },
  "color_config": "aces_1.2"
}
```

### document/main.ron (RON形式、フォーマット v7)

現行フォーマットの主体。`Document`（`ravel-core::composition::Document`）全体を
pretty RON で永続化する: レガシー平坦グラフ、全 Composition/Layer（各レイヤーの
ネットワーク・シェルプロパティ・予約フィールド含む）、メディアアセット
（`MediaAssetEntry`。v4 で相対 / 変数パス対応）、公開パラメータ宣言
（`exposed_parameters`。v7 で追加）。
`compositions`/`media_assets` は ID・キー昇順にソートされ決定的出力となるため git diff
が有効。`exposed_parameters` は宣言順そのものが提示順（＝データ）なので
`Vec` としてそのまま並び、ソートしない。読み込み後は `Document::advance_id_counters()` で全 ID カウンタを文書の最大
ID 超に進める（REQ-LAYER-009）。

`Layer.audio` は format v4 への追加フィールドとして同居し、migration は
追加しない。既存 v4 の欠落フィールドは `None`、各 `AudioSource` フィールドの
欠落は serde default で補う。`struct_names(true)` の RON では値を
`Some(AudioSource(...))` として永続化する。

format v5 はノードのベクタパラメータを `_x` / `_y` の Float 対から
`Channel2` / `Channel3` の 1 パラメータへ畳んだ。RON の構造自体は変わらない
（パラメータは自由なキー / 値の対）ので、移行はロード後の型付きパス
`Document::fold_component_params()` が担う（`manifest.json` の連鎖ではない）。

format v6 は `field.curve_remap` の制御点を `"0:0,1:1"` 文字列から
`ParameterValue::Curve` へ変えた。理由も形も v5 と同じで、移行は
`Document::upgrade_curve_params()` が担う。旧リーダーと同じく読めない要素は
1 つずつ捨て、読める点が 0 個のときだけ恒等カーブ（0:0, 1:1）へフォールバック
する。捨てたものは警告に出す。

format v7 は `Document.exposed_parameters`（公開パラメータ宣言、REQ-PROJ-006）を
追加した。**追加フィールドだけ**なので移行は型付きパスを持たず、v6 以前の文書は
`#[serde(default)]` で「宣言ゼロ」として読める。それでも版を上げるのは、宣言が
別のツールが名前で読む契約で、旧ビルドが黙って捨てて書き戻すと画面上は何も
変わらないまま契約が消えるため（判断基準は
[`../dev/persistence.md`](../dev/persistence.md)）。

### graph/main.ron (RON形式、フォーマット v1–v2)

`GraphDoc`（`ravel-app::project::graph_doc`）として永続化。ライブ`Graph`から
`NodeId`/`EdgeId`昇順でソートした`Vec`に射影し、決定的出力でgit diffを有効化。
ノードは入出力ポート (`inputs`/`outputs`) とエディタ用メタデータ (`metadata`) を保持。
v3 以降は書き込まれず、旧アーカイブの読み込み時に `Document::graph` へ包まれて
マイグレーションされる（評価対象はルート Composition のレイヤーネットワーク）。

```ron
GraphDoc(
  nodes: [
    Node(
      id: NodeId(1),
      type_key: "read_media",
      inputs: [],
      outputs: [
        OutputPort(name: "out", data_type: DataTypeId(1)),
      ],
      metadata: NodeMetadata(label: None, position: (100.0, 200.0), collapsed: false),
    ),
    Node(
      id: NodeId(2),
      type_key: "color_correct",
      inputs: [
        InputPort(name: "in", accepted_types: [DataTypeId(1)]),
      ],
      outputs: [
        OutputPort(name: "out", data_type: DataTypeId(1)),
      ],
      metadata: NodeMetadata(label: None, position: (300.0, 200.0), collapsed: false),
    ),
    Node(
      id: NodeId(3),
      type_key: "sequence",
      inputs: [
        InputPort(name: "in", accepted_types: [DataTypeId(1)]),
      ],
      outputs: [],
      metadata: NodeMetadata(label: None, position: (500.0, 200.0), collapsed: false),
    ),
  ],
  edges: [
    Edge(id: EdgeId(1), source: NodeId(1), source_port: OutputPortIndex(0), target: NodeId(2), target_port: InputPortIndex(0)),
    Edge(id: EdgeId(2), source: NodeId(2), source_port: OutputPortIndex(0), target: NodeId(3), target_port: InputPortIndex(0)),
  ],
)
```

> ノードパラメータ（`gain`/`gamma`等の値・アニメーションチャネル）は
> `Node::parameters` としてモデル化済みで、Graph/Document の RON に含まれる。

### assets/refs.json（v4 で廃止）

アセット参照は `document/main.ron` の `media_assets` に一本化した。v3 以前も
このエントリは**常に空のコレクション**しか書いていないため、残っている
アーカイブを開いても情報は失われない — 単に無視する。

### ui_state.json (UI 状態、REQ-UI-013)

```json
{
  "active_comp": 2
}
```

ユーザーが「何を見ていたか」はドキュメントの一部ではない — アクティブコンプを
`Document` に入れると undo スナップショット（undo の単位）に載ってしまい、
編集を戻すとコンプ切替まで巻き戻る。そのため独立エントリに置く。

このエントリは**読み書きとも任意**である。エントリを持たないアーカイブは
デフォルト値で読め（アクティブコンプは `Document::root_comp` にフォールバック）、
新しい Ravel が書いた未知のフィールドは無視する。壊れて読めないエントリも
警告ログを出してデフォルトに縮退する — ユーザーデータを持たないエントリのために
無傷のプロジェクトを開けなくしない。したがって
このエントリ自体は `format_version` を上げない（追加時も据え置きだった）。将来の UI 永続状態（Outliner の展開集合、
Node Editor のビュー位置など）もここに集約する。

### settings.toml (プロジェクトオーバーライド)

```toml
locale = "ja"

[appearance]
theme_mode = "system"
light_theme = "Ravel Light"
dark_theme = "Ravel Dark"

[color]
ocio_config = "./ocio/config.ocio"
working_space = "ACEScg"
display_space = "sRGB"

[playback]
frame_rate = "30"
proxy_mode = "auto"
proxy_resolution = 0.5

[auto_save]
enabled = true
interval_seconds = 120

[cache]
vram_limit_mb = 1024
ram_limit_mb = 2048
disk_limit_mb = 4096
sim_reserve_ratio = 0.25
disk_enabled = false
```

`[appearance]` は UI の外観。`theme_mode` は `system` / `light` / `dark`
（既定は `system` = OS 追従）で、`light_theme` / `dark_theme` は
テーマレジストリのテーマ名（既定は同梱の `Ravel Light` / `Ravel Dark`）。
2 つのテーマ名を別に持つのは、モードを切り替えても他方の選択が消えないため。
存在しないテーマ名は適用時に同梱テーマへフォールバックし、設定値は要求された
名前を保持する（テーマディレクトリは非同期に読まれるので、後から現れた
テーマを忘れないため）。

`[cache]` は `CacheBudget`（`ravel_core::cache_budget`）の上限で、
`default → global → project → user` の 4 段マージに他の節と同じ形で乗る。
全フィールド任意で、既定値は `CacheBudgetConfig` の定数が正
（設定側に数値を二重に持たない）。

- `vram_limit_mb` は VRAM の**総額**である。キャッシュが保持している
  テクスチャとテクスチャプールのアイドル枠の合計で、アイドル枠は残余
  （総額 − 保持分）として動的に決まる。プールは自分の上限を持たない
- `ram_limit_mb` は評価結果キャッシュなどホスト側の総額
- `sim_reserve_ratio` は各層でシミュレーション状態のために確保する割合。
  通常エントリはこの枠を使えず、通常エントリの圧力で sim が退避されることも
  ない
- `root` / `disk_limit_mb` / `disk_enabled` はディスク層の設定。**層の実装は
  未実装で、担当は `docs/implementation/cache-plan.md` の `CACHE-11`。**
  `disk_enabled = false`（既定）では割り当ては 0 になる
- **この節はまだ実行時に届かない。** パースとマージは他の節と同じように
  動くが、`Project::resolved_settings` を呼ぶ本番コードが存在しない
  （設定レイヤー全体の未接続。`issues/medium/app-shell.md` の `MED-APP-10`)。
  起動時の予算は `CacheBudgetConfig` の既定値から作られ、ファイルに書いた値は
  無視される。解決済みの設定を走行中の予算へ流す
  （`SharedCacheBudget::reconfigure`）配線と設定画面からの編集は、どちらも
  `docs/implementation/settings-screen-plan.md` の `SET-8` が担当する

## 制約・前提条件

- 全内部処理は32bit float
- RON形式はRustネイティブでパース/シリアライズが高速
- プロジェクトファイルはgit diffが有効なテキスト形式
- アセットはプロジェクト内に埋め込まず参照のみ保持
- 関連要件: REQ-CORE-001, REQ-CORE-003, REQ-CORE-007, REQ-PROJ-001
