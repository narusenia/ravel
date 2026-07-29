# Ravel — アーキテクチャ仕様書

## 概要

Ravelは「ノードグラフファースト」のアーキテクチャ。全てのデータフロー、エフェクト、合成処理がDAG（有向非巡回グラフ）上のノード接続として表現される。タイムラインはこのDAG上の糖衣表現（シーケンスノード）として実装。UI層と処理層は明確に分離され、GPUIによるUI描画とwgpuベースのGPU計算パイプラインがGPUコンテキストを共有する。

## レイヤー構成

```
┌─────────────────────────────────────────────────────────┐
│                    UI Layer (GPUI)                       │
│  ┌──────────┐ ┌──────────┐ ┌────────┐ ┌─────────────┐  │
│  │ Timeline │ │NodeGraph │ │ Viewer │ │ Properties  │  │
│  │  Editor  │ │  Editor  │ │+Scopes │ │  Inspector  │  │
│  └────┬─────┘ └────┬─────┘ └───┬────┘ └──────┬──────┘  │
│       └─────────────┴───────────┴─────────────┘         │
│                         │ Commands / Queries             │
├─────────────────────────┼───────────────────────────────┤
│                  Application Layer                       │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐  │
│  │  Undo    │ │Workspace │ │  i18n    │ │  Config   │  │
│  │ Manager  │ │ Manager  │ │  System  │ │  Manager  │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────┘  │
├─────────────────────────┼───────────────────────────────┤
│                    Core Engine                           │
│  ┌──────────────────────────────────────────────────┐   │
│  │              DAG Evaluation Engine                │   │
│  │  ┌────────┐ ┌──────────┐ ┌───────────────────┐  │   │
│  │  │ Graph  │ │  Node    │ │  Cache Manager    │  │   │
│  │  │Manager │ │Evaluator │ │ (VRAM/RAM/Disk)   │  │   │
│  │  └────────┘ └──────────┘ └───────────────────┘  │   │
│  └──────────────────────────────────────────────────┘   │
│  ┌─────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐   │
│  │  Type   │ │Animation │ │  Lua     │ │  Plugin   │   │
│  │ System  │ │ Channel  │ │ Runtime  │ │  Host     │   │
│  └─────────┘ └──────────┘ └──────────┘ └───────────┘   │
├─────────────────────────────────────────────────────────┤
│                   Media Layer                            │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐  │
│  │  FFmpeg  │ │HW Decode │ │  Audio   │ │   OCIO    │  │
│  │ Backend  │ │ Backend  │ │  Engine  │ │  Backend  │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────┘  │
├─────────────────────────────────────────────────────────┤
│                    GPU Layer                              │
│  ┌──────────────────────────────────────────────────┐   │
│  │              wgpu Compute Pipeline                │   │
│  │  ┌────────┐ ┌──────────┐ ┌───────────────────┐  │   │
│  │  │Shader  │ │ Texture  │ │  Native API       │  │   │
│  │  │Manager │ │  Pool    │ │  Fallthrough      │  │   │
│  │  └────────┘ └──────────┘ └───────────────────┘  │   │
│  └──────────────────────────────────────────────────┘   │
├─────────────────────────────────────────────────────────┤
│                  Platform Layer                           │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐  │
│  │  macOS   │ │ Windows  │ │  Linux   │ │   File    │  │
│  │ (Metal)  │ │ (D3D11)  │ │(Vulkan)  │ │  System   │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────┘  │
└─────────────────────────────────────────────────────────┘
```

## コアエンジン詳細設計

### DAG評価エンジン

**評価モデル: Hybrid Pull + Dirty Notification**

```
パラメータ変更
    │
    ▼
 Dirty伝播 (Push)
    │ 下流ノードのdirtyフラグをON
    ▼
 出力ノードからPull評価要求
    │
    ▼
 各ノードを再帰的に評価
    │ dirtyフラグがOFF → キャッシュ返却
    │ dirtyフラグがON  → 再評価 → キャッシュ更新
    ▼
 結果をビューアに表示
```

**ノード評価の疑似コード**（レイヤーネットワークモデル v3、REQ-LAYER-007）:
```rust
// 実装シグネチャ:
//   NodeProcessor::process(&self, node, ctx, inputs, params, scope)
//     inputs: &[Option<Arc<dyn NodeData>>]  — 入力ポート順スロット（未接続は None）
//     params: &ResolvedParams               — フレーム解決済みパラメータ（REQ-LAYER-004）
//     scope:  &mut dyn EvalScope            — サブグラフ再帰評価・Document 参照
fn evaluate(&self, path: &[PathSegment], node_id: NodeId, frame: Frame, ctx: &EvalContext)
    -> Arc<dyn NodeOutput>
{
    // キャッシュチェック（キーは所有パス + NodeId。REQ-LAYER-009）
    let key = (path, node_id);
    if let Some(cached) = ctx.cache.get(key, frame) {
        if !ctx.dirty_set.contains(key) {
            return cached;
        }
    }

    // 入力の再帰評価（target入力ポート index 昇順で整列、多出力ノードは
    // PortRecord から source_port で抽出）
    let inputs: Vec<Option<Arc<dyn NodeOutput>>> = self.graph
        .inputs(node_id)
        .map(|(input_id, source_port)| self.evaluate(path, input_id, frame, ctx).extract(source_port))
        .collect();

    // パラメータの評価時解決（定数・キーフレーム・ノード出力バインド）
    let params = self.resolve_params(node_id, frame, ctx);

    // パラメータポート（InputPort.is_param）: 接続された入力を inputs から
    // 分離（プロセッサには渡さない）し、型変換して params へ上書き。
    // 優先順位: attribute > pin > parameter（REQ-LAYER-008 の一般化）。
    // 未接続・変換不能は stored パラメータへフォールバック。
    overlay_param_ports(&mut inputs, &mut params, node);

    // ノード処理実行（プロセッサは Evaluator がノードごとに登録・保持）
    let result = self.processor(node_id).process(node, ctx, &inputs, &params, scope);

    // キャッシュ更新 & dirtyクリア
    ctx.cache.put(key, frame, result.clone());
    ctx.dirty_set.remove(key);

    result
}
```

**ネットワークスコープ（v3）**: レイヤーネットワーク・サブネットワークの
評価は `EvalScope::evaluate_sub(segment, graph, output, ctx, bindings)` で
再帰する。`segment`（`Layer(comp, layer)` / `Subnet(node)` / 予約 `Comp`）
が評価パスに積まれ、キャッシュ/dirty はパスで名前空間化される。境界
ノード（`comp.network`）は EvalContext をレイヤーローカル時間に書き換えて
渡し、Evaluator は Document を保持してレイヤーのネットワークを解決する
（Document-aware）。スコープの無効化（`invalidate_scope`）はオーナー
ノードのキャッシュも道連れにし、ネットワーク編集が殻チェーンへ自動伝播する。

### 型システム

```rust
// 基本トレイト
trait NodeData: Send + Sync + 'static {
    fn data_type_id(&self) -> DataTypeId;
    fn as_any(&self) -> &dyn std::any::Any; // 入力を具体型へ downcast するため
}

// カテゴリトレイト
trait BufferData: NodeData {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn pixel_format(&self) -> PixelFormat;
}

trait TemporalData: NodeData {
    fn duration(&self) -> Duration;
    fn frame_rate(&self) -> FrameRate;
}

trait GeometricData: NodeData {
    fn bounds(&self) -> Rect;
    fn transform(&self) -> Transform2D;
}

// 具体型
struct FrameBuffer { /* RGBA f32 */ }
struct Clip { /* フレーム列 + メタデータ */ }
struct Shape { /* 2Dパスデータ */ }
struct Scalar(f32);
struct Vec2(f32, f32);
struct Color(f32, f32, f32, f32);
struct AudioBuffer { /* PCM f32 */ }
struct ParticleSystem { /* パーティクル群 */ }
// ...

impl BufferData for FrameBuffer { /* ... */ }
impl TemporalData for Clip { /* ... */ }
impl GeometricData for Shape { /* ... */ }
```

### イミュータブルグラフ + アンドゥ

```rust
struct GraphVersion {
    nodes: im::HashMap<NodeId, Arc<Node>>,
    edges: im::HashMap<EdgeId, Edge>,
    // im クレートのpersistent data structureで構造共有
}

struct UndoStack {
    versions: Vec<Arc<GraphVersion>>,
    current: usize,
}

impl UndoStack {
    fn push(&mut self, new_version: Arc<GraphVersion>) {
        self.versions.truncate(self.current + 1);
        self.versions.push(new_version);
        self.current += 1;
    }

    fn undo(&mut self) -> Option<&Arc<GraphVersion>> {
        if self.current > 0 {
            self.current -= 1;
            Some(&self.versions[self.current])
        } else {
            None
        }
    }
}
```

### 統一アニメーションチャネル

```rust
enum ChannelSource {
    Constant(f32),
    Keyframes(KeyframeCurve),       // ベジエ/リニア/ステップ
    Expression(LuaExpression),       // Luaスクリプト
    NodeOutput(NodeId, OutputPort),  // 他ノードの出力
    AudioReactive(AudioAnalysisRef), // オーディオ解析
    Blend(Box<ChannelSource>, Box<ChannelSource>, BlendMode, f32),
}

struct AnimationChannel {
    source: ChannelSource,
}

impl AnimationChannel {
    fn evaluate(&self, frame: Frame, ctx: &EvalContext) -> f32 {
        match &self.source {
            ChannelSource::Constant(v) => *v,
            ChannelSource::Keyframes(curve) => curve.sample(frame),
            ChannelSource::Expression(expr) => expr.eval(frame, ctx),
            ChannelSource::NodeOutput(id, port) => ctx.get_output(*id, *port, frame),
            ChannelSource::AudioReactive(r) => r.sample(frame, ctx),
            ChannelSource::Blend(a, b, mode, factor) => {
                mode.blend(a.evaluate(frame, ctx), b.evaluate(frame, ctx), *factor)
            }
        }
    }
}
```

## スレッディングモデル

```
┌──────────────┐
│  UI Thread   │ ← GPUIメインループ、入力処理、描画
│  (GPUI)      │
└──────┬───────┘
       │ crossbeam-channel (ロックフリー)
┌──────┴───────┐
│  Eval Pool   │ ← ノードグラフ評価、エフェクト処理
│  (rayon)     │   CPU並列はrayonのwork-stealing
└──────┬───────┘
       │ GPUコマンドバッファ投入
┌──────┴───────┐
│  GPU Thread  │ ← wgpuコマンド投入、シェーダディスパッチ
└──────────────┘
┌──────────────┐
│ Decode Pool  │ ← FFmpegデコード、HWデコーダ制御
└──────────────┘
┌──────────────┐
│ Audio SRC    │ ← 全長トラックのサンプルレート変換
│  Worker      │   世代付き結果をAudio Prepへ返す
└──────────────┘
┌──────────────┐
│ Audio Prep   │ ← ミキシング、エフェクト処理
│  Thread      │   epoch付きchunkをCPALコールバックへ送信
└──────────────┘
┌──────────────┐
│ Audio CPAL   │ ← リアルタイム優先度、CPAL callback
│  Callback    │   ※ 絶対にブロックしない（try_recvのみ）
└──────────────┘
┌──────────────┐
│ Tokio Runtime│ ← ファイルI/O、ネットワーク、プラグインホスト
└──────────────┘
┌──────────────┐
│ OFX Process  │ ← 子プロセス（プラグイン隔離実行）
│  (separate)  │
└──────────────┘
```

### 再生クロック

再生位置の正は状況で切り替わる（`docs/implementation/audio-plan.md` 決定 4、
実装は `crates/ravel-app/src/audio/` と `src/playback.rs`）:

- **アクティブコンプに音声トラックがあり出力デバイスが開けたとき**:
  CPAL コールバックが出力したサンプル数だけ `SyncClock` を進め、
  UI スレッドのフレーム間隔タイマが `Transport::tick_with(ClockSource::Audio)`
  でそのサンプル位置から表示フレームを読む。音声が途切れない側にクロックを
  合わせるため、長時間再生でドリフトする `Instant` には従わない。
- **それ以外（音声トラック 0 本、デバイス無し、CI・ヘッドレステスト）**:
  従来どおり `ClockSource::Wall(Instant)` で `PlaybackClock` が
  `base_frame + 経過時間 × fps` を閉形式で計算する。
- 切り替えの判定は `audio::playback_clock` 1 箇所。play / pause / seek は
  常に `AudioEngine` にも転送され、クロック切替時に再生位置が跳ばない。
- CPAL コールバックは再生中のみ `SyncClock` を進める（ポーズ中の
  無音出力では進めない）。seek / pause は epoch を更新し、コールバックが
  保持中またはキュー内の旧 epoch chunk を破棄する。アンダーラン時は実際に
  chunk からコピーできたフレーム数だけ進める。epoch / clock の transport 更新と
  callback の clock commit は atomic gate で直列化し、callback は gate 取得に
  失敗した場合にブロックせず無音を返す。
- 出力レート、チャンネル数、sample format は既定デバイスの supported default
  config を採用し、同じ設定をミキサ、`SyncClock`、CPAL stream に渡す。
  ミックスは prep スレッド側、全長 SRC は専用 worker で行う。SRC job は track
  ごとに最新一件へ集約し、旧世代と shutdown を処理 block 境界で取り消す。
  コールバックは非ブロッキング受信と sample format 変換だけを行う。

## キャッシュアーキテクチャ

三層キャッシュ（REQ-CORE-006）は**出力段のフレーム**に対して定義される。
ノード単位の評価キャッシュは 1 ノード 1 値で、同一性の照合によって
有効性を判断する層として別に存在する。

```
要求: (所有パス, TimeKey, 解像度, fps, 品質, 最低精度)
                    │
┌───────────────────▼───────────────────────────────────┐
│  フレームキャッシュ（出力段: Composition / レイヤー出力）│
│    VRAM: GPU テクスチャ（f16）— ゼロコピー表示           │
│    RAM : f16 バイト列 — CPU 消費時のみ f32 展開          │
│    Disk: グローバルなキャッシュディレクトリ（設定可変）    │
└───────────────────┬───────────────────────────────────┘
              ミス │
┌───────────────────▼───────────────────────────────────┐
│  Evaluator（ノード単位 1 エントリ + 同一性照合）         │
│    sim キャッシュは別 map（保護枠を持つ）                │
└───────────────────┬───────────────────────────────────┘
                    │ メディア読み
┌───────────────────▼───────────────────────────────────┐
│  共有デコードフレームキャッシュ（アセット単位）           │
└───────────────────────────────────────────────────────┘

全層が単一のバイト予算の下で会計され、ヒット率とキャッシュ済み範囲を
API から観測できる
```

同一性は量子化した時間（`TimeKey`）・解像度・fps・品質・精度・bypass の組。
照合規則は 2 種類だけで、混ぜない:

- **精度以外は厳密一致** — 時間・解像度・fps・品質・bypass。
- **精度は順序比較** — 要求側が最低精度を宣言し、保存精度がそれ以上なら
  ヒット（無損失でそのまま渡せる）、下回ればミス。書き出しは常に f32 を
  要求する。

エントリを縮小・降格して流用する近似ヒットは行わない。画素演算は常に f32 で、
縮約された保存表現は演算へ渡る境界で f32 として読み出す（REQ-CORE-009）。

> **実装状況**: 現時点で存在するのはノード単位の評価キャッシュ
> （`ravel-core/src/eval.rs`。バイト上限なし）、シェーダ / パイプライン
> キャッシュ、テクスチャプール、メディアのデコーダ・静止画キャッシュ、
> サムネイルのディスクキャッシュ。三層のフレームキャッシュ・単一予算・
> ヒット率の API は未実装で、設計と実装単位は
> `docs/implementation/cache-plan.md` にある。

## プロジェクトファイル構造

```
project.ravprj (zip)
├── manifest.json            # フォーマットバージョン、メタデータ
├── graph/
│   ├── main.ron             # ルートノードグラフ定義
│   └── subgraphs/
│       ├── color_grade.ron  # サブグラフ定義 (Group or Comp)
│       └── intro_effect.ron # Comp: 独自解像度/FPS/尺を持つ
├── assets/
│   └── refs.json            # v4 で廃止（常に空だった。読み飛ばす）
├── presets/
│   └── node_presets.ron     # ノード単位プリセット
├── settings.toml            # プロジェクト固有設定オーバーライド
├── ui_state.json            # UI 状態（アクティブコンプ等、任意エントリ）
└── .journal/                # 操作ジャーナル（正常終了時コンパクション）
```

派生キャッシュ（サムネイル・波形・フレーム）はコンテナ内に置かず、
グローバルなキャッシュディレクトリに置く（REQ-CORE-006、REQ-PROJ-001）。

```
<config_base>/ravel/cache/    # 格納先は設定で変更可能
├── thumbnails/
├── waveforms/
└── frames/<project-key>/
```

> **実装状況**: 現行実装（`ravel-app/src/project/container.rs`）が読み書きするのは
> `manifest.json` / `document/main.ron` / `settings.toml` /
> `ui_state.json`（フォーマット v4）。`ui_state.json` は任意エントリで、
> 欠落時は既定値で読むため、この UI 状態の追加時には format_version 3 を
> 据え置いた（REQ-UI-013）。`document/main.ron` は Composition/Layer・各
> レイヤーネットワーク・`Layer.audio`・メディアアセット（`MediaAssetEntry`。
> v4 で相対 / 変数パス対応、REQ-PROJ-001）を含む `Document` 全体の RON。
> `Layer.audio` は v4 への加算的フィールドで、v5 や migration は追加しない。
> `assets/refs.json` は v4 で廃止済みで、レガシーアーカイブに残っていても読み飛ばす。
> v1–v2 の `graph/main.ron`（レガシー平坦グラフ）は読み込み時のマイグレーション専用。
> `subgraphs/`・`presets/`・`.journal/` は将来拡張。グローバルなキャッシュ
> ディレクトリのうち実装済みは `thumbnails/` のみ
> （`ravel-app/src/media/cache.rs`）。

## OpenFX統合アーキテクチャ

```
┌──────────────────────────────────┐
│         Ravel Main Process       │
│                                  │
│  ┌────────────────────────────┐  │
│  │      OFX Host Shim        │  │
│  │  (Rust → C/C++ FFI)       │  │
│  │                            │  │
│  │  ┌──────────────────────┐  │  │
│  │  │   Suite Registry     │  │  │
│  │  │  - Image Effect ✓    │  │  │
│  │  │  - Parameter ✓       │  │  │
│  │  │  - GPU Render ✓      │  │  │
│  │  │  - Multi-clip (将来) │  │  │
│  │  │  - Temporal (将来)   │  │  │
│  │  │  - Interact (将来)   │  │  │
│  │  └──────────────────────┘  │  │
│  └──────────┬─────────────────┘  │
│             │ IPC (shared mem)   │
├─────────────┼────────────────────┤
│             ▼                    │
│  ┌────────────────────────────┐  │
│  │    OFX Plugin Process     │  │ ← 子プロセス（隔離）
│  │  - プラグインDLLロード      │  │
│  │  - renderAction実行        │  │
│  │  - クラッシュ時自動再起動    │  │
│  └────────────────────────────┘  │
└──────────────────────────────────┘
```

## カラーマネジメントパイプライン

```
入力メディア → [入力カラースペース変換 (OCIO)] → 作業空間 (32bit float リニア)
                                                          │
                                                  ノード評価
                                                          │
                                                          ▼
作業空間 → [表示カラースペース変換 (GPU 3D LUT)] → ビューア表示
                                                          │
作業空間 → [出力カラースペース変換 (OCIO)] → エンコード → ファイル出力
```

- 全内部処理は32bit floatリニア空間
- OCIO `.ocio`設定でカラースペース変換を定義
- ビューア表示用はGPU 3D LUTにベイクしwgpuシェーダで適用
- LUT再生成は設定変更時のみ（フレーム毎ではない）

## 制約・前提条件

- GPUIのwgpuカスタムフォーク依存（Zed upstream追従が必要）
- FFmpegはLGPLダイナミックリンク（静的リンク不可）
- OCIOはC++ライブラリ（FFIコスト、ビルド複雑度）
- OFXプラグインはC ABI（型安全性なし、プロセス分離で安全性確保）
- オーディオスレッドはリアルタイム制約（ヒープアロケーション/ロック禁止）
- macOSリード開発のため、Windowsでの動作確認は設計段階からCI含めて行う
