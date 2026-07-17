# レイヤーネットワークモデル実装計画

対応要件: `docs/requirements/REQ-LAYER.md`（REQ-LAYER-001 〜 011）、
REQ-CORE-001 (v3)、REQ-UI-003 (v3)。

## 背景

現行のタイムラインモデルは After Effects 風の Composition/Layer で、
Layer は `LayerSource` enum による構造分岐 + ビルトイン Transform +
`effect_graph: Option<Graph>` の二重構造を持つ。評価は
`compile_composition` が Composition 全体を単一の平坦グラフに展開する
コンパイラ方式を想定していたが、以下の問題がある。

- レイヤー展開が `Source → TimeOffset → Effects → Transform → Opacity →
  Merge` の直列チェーンと 7 種固定の `NodeRole` にハードコードされ、
  レイヤー内で自由な分岐ネットワークを表現できない
- 決定論的 ID が `comp(32bit) | layer(24bit) | role(8bit)` のビット
  パッキングで、レイヤー内の任意ノード数を表現できない
- ノードパラメータは静的値のみで、キーフレームは Layer の
  transform/opacity にしか存在しない
- evaluator にノードごとの EvalContext 書き換え機構がなく、
  レイヤーローカル時間を表現できない（`TimeOffsetProcessor` は
  pass-through）
- `compile_composition` はアプリに配線されておらず、実行系はノード
  エディタの平坦 Graph 直評価の過渡期にある

これらを解消し、「1 タイムラインレイヤー = 1 ノードネットワーク」
（Houdini 的）のモデルに移行する。

## 目的

- 全レイヤーが殻（汎用プロパティ）+ 所有するノードネットワークの
  同一構造になる
- ネットワーク内の任意パラメータがキーフレーム可能になる
- レイヤー間参照（Layer Ref）と Null レイヤーによる Geometry/Field の
  供給が可能になる
- ネットワークが常にレイヤーローカル時間で評価される
- 既存資産（イミュータブル Graph、dirty 通知 evaluator、Geometry/Field
  システム、ノードエディタ）を最大限再利用する

## 目標アーキテクチャ

```text
Document
 ├─ compositions: Map<CompId, Composition>
 │    └─ Composition (resolution / fps / duration)
 │         └─ layers: Vec<Layer>            ← 殻: 時間配置 / Transform /
 │            │                                 Opacity / blend_mode /
 │            │                                 親子付け / adjustment
 │            └─ network: Graph             ← Layer が所有（入れ子構造）
 │                 ├─ In   (base_geometry, t, カスタムパラメータ)
 │                 ├─ ... ユーザーノード / Subnet(Graph) / Layer Ref ...
 │                 └─ Out  (frame: FRAME_BUFFER + カスタムポート)
 └─ root_comp

評価フロー:

Viewer (root comp 出力を常時評価)
  └─ 殻コンパイル（synthetic、決定論的 ID）
       [時間変換] → [ネットワーク境界] → [Transform] → [Opacity] → [Merge]
                        │
                        ▼ EvalContext をローカル時間に書き換え
                   network.Out.frame を再帰 pull
                        ├─ Subnet   : 同じ機構の再帰
                        └─ Layer Ref: Document から参照先レイヤーの
                                      ネットワークを解決して再帰 pull
```

- 所有パス `CompId / LayerId / [SubnetNodeId ...] / NodeId` が
  ネットワーク内ノードのグローバル一意性と評価キャッシュキー
  （ハッシュ）を担う
- 殻の決定論的 ID（`comp | layer | role` ビットパッキング）は殻の
  synthetic ノード専用に縮小して継続使用
- evaluator は Graph 単体ではなく Document（Composition 解決
  コンテキスト）を受け取る

## Phase 1: モデルと評価基盤

UI を伴わないコア層の再定義。ゴールデンテストで検証する。

### 主な対象

- `crates/ravel-core/src/composition/mod.rs`（Layer / Document）
- `crates/ravel-core/src/composition/compile.rs`（旧 role 展開の解体）
- `crates/ravel-core/src/graph.rs`（動的ポート、パラメータ値ソース）
- `crates/ravel-core/src/eval.rs`（再帰評価、スコープ付き EvalContext）
- `crates/ravel-core/src/animation/`（パラメータへのチャネル統合）

### 作業

- **Layer 再定義**: `network: Graph` を所有する構造に変更。
  `LayerSource` enum と `effect_graph` を削除。adjustment フラグ追加。
  予約フィールド（`time_remap`、`track_matte`）を追加
- **所有パス ID**: `CompId / LayerId / [SubnetNodeId ...] / NodeId` の
  パス型を定義し、評価キャッシュ・dirty 集合のキーとする。ノード ID は
  ドキュメント内でグローバル一意（`NodeId::next` 採番、永続化は
  読み込み時にこの不変条件を維持）とし、所有パスは ID 衝突のためでは
  なく、同一グラフが複数のオーナー経由で評価される際のインスタンス
  区別に使う
- **In / Out ノード**: 動的ポート（カスタムパラメータ、カスタム出力
  ポート）を持つノード型を Graph に導入。In の固定ポート
  `base_geometry` / `t`
- **パラメータの値ソース**: `ParameterValue` を拡張し、ChannelSource
  （Constant / Keyframes / NodeOutput / Blend）を持てるようにする。
  Vec/Color はコンポーネント別チャネル
- **評価時パラメータ解決**: プロセッサが構築時キャプチャではなく、
  `process()` にフレーム解決済みの値を受け取るインターフェースに変更
- **再帰評価**: ネットワーク境界ノードのプロセッサが内部 Graph の
  Out を pull する機構。入力値の内部 In への束縛。スコープ付き
  EvalContext（ローカル時間書き換え）
- **Document-aware evaluator**: 評価器が Composition 解決コンテキストを
  受け取る形に変更

### 完了条件

- 新モデルの Layer（殻 + ネットワーク）を構築し、Out の `frame` を
  評価できるゴールデンテストがある（`shape_layer_golden.rs` を新モデル
  に移植）
- ローカル時間評価・カスタムパラメータ・キーフレーム付きパラメータの
  評価テストがある
- `LayerSource` / `effect_graph` / `comp.source.*` / 旧 role 展開が
  削除されている
- `mise run check` が通る

## Phase 2: ノードと殻コンパイラ

### 主な対象

- `crates/ravel-core/src/composition/compile.rs`（殻チェーン再設計）
- `crates/ravel-nodes/src/comp/`（synthetic プロセッサ群の整理）
- `crates/ravel-nodes/src/rasterize.rs`（color 入力）
- `crates/ravel-nodes` ↔ `crates/ravel-media`（Video ノードの橋）

### 作業

- **殻チェーン再設計**: `[時間変換+ネットワーク境界] → Transform →
  Opacity → Merge` の synthetic 生成。adjustment レイヤーの
  `background' = network(background)` 分岐。solo/mute プレパス、
  親子付け Transform エッジの維持。旧 `comp.time_offset` は境界ノード
  に吸収
- **Rasterize の color 入力**: `color` ピン追加。`CD` / `ALPHA` 属性
  優先 → `color` ピン → デフォルト色パラメータの意味論
- **テンプレート定義**: Solid / Video / Shape / Null の初期ネットワーク
  をデータ駆動で定義（コード埋め込みにしない。将来のユーザー定義
  テンプレートに備える）
- **Video ノード**: ravel-media のデコードをノードプロセッサに接続。
  `asset_id` パラメータ、ローカル時間でフレーム要求
- **Layer Ref ノード**: 参照先レイヤー + ポート名パラメータ。
  Document 経由の解決。循環参照検出（`validate.rs` に Layer Ref 循環を
  追加）
- **サブネットノード**: 内部 Graph 所有、In/Out インターフェース、
  未接続ピンのパラメータ露出

### 完了条件

- Solid / Shape / Video / Null / 調整レイヤー / サブネット / Layer Ref
  の評価テストが通る
- Layer Ref 循環参照が検出・拒否されるテストがある
- Rasterize の属性優先（CD/ALPHA > color ピン > デフォルト）テストが
  ある
- 異 fps メディア/PreComp が秒ベースで正しくマッピングされるテストが
  ある

## Phase 3: UI 統合

### 主な対象

- `crates/ravel-app/src/panels/node_editor.rs`（コンテキスト化）
- `crates/ravel-app/src/panels/timeline.rs`（Document 駆動化）
- `crates/ravel-app/src/panels/viewer.rs`（root comp 常時評価）
- `crates/ravel-app/src/panels/properties.rs`（カスタムパラメータ）
- `crates/ravel-ui`（コマンド、プロパティ定義）

### 作業

- **ノードエディタのコンテキスト化**: 編集中のネットワークを所有パスで
  保持。タイムラインからの「ネットワークを開く」、サブネットへの潜り、
  パンくずバー。`NodeMetadata.synthetic` の非表示フィルタを実装
- **テンプレートからのレイヤー作成**: Solid / Video / Shape / Null の
  作成コマンドをテンプレート定義から生成
- **Viewer の切替**: 「選択ノードの評価」から root comp 出力の常時評価
  へ（EvalService の要求対象変更）。選択ノードの単独プレビューは
  ネットワークコンテキスト付きで維持。**PlaybackController の評価要求も
  Document-aware evaluator の root comp 出力へ更新する**（後述の
  「関連計画との依存」を参照）
- **プロパティパネル**: 殻の属性 + In のカスタムパラメータの表示・編集
- **タイムラインの Document 駆動化**: パネルローカルのデモ Composition
  を廃止し、Document の Composition を表示・編集。undo を Document
  単位に統合

### 完了条件

- アプリ上でレイヤー作成 → ネットワーク編集 → Viewer 表示が通る
- synthetic ノードがノードエディタに表示されない
- タイムラインでレイヤー選択してもノードエディタのコンテキストが
  維持される
- レイヤーの追加・削除・並べ替え・トリムが Document 単位 undo で
  巻き戻せる

## Phase 4: アニメーション UI と永続化

### 主な対象

- `crates/ravel-app/src/panels/timeline.rs`（プロパティツリー、
  キーフレーム編集）
- `crates/ravel-app/src/project/`（永続化フォーマット）

### 作業

- **タイムラインのプロパティツリー**: 殻の Transform/Opacity に加え、
  キーフレームを持つネットワーク内パラメータを列挙
- **キーフレーム編集**: キーフレームダイヤの追加・移動・削除
- **プロジェクト保存**: Composition + ネットワーク（入れ子 Graph）+
  予約フィールドを含む永続化フォーマット。将来のネットワーク独立
  リソース化（HDA 共有）を潰さない構造
- **マイグレーション**: 旧フォーマット（Graph のみの RON）からの
  移行方針

### 完了条件

- ネットワーク内パラメータにキーフレームを打って再生できる
- 保存 / 読込でプロジェクトが復元できる
- 予約フィールドを含むラウンドトリップテストがある

## 非対象

本計画では以下を変更・実装しない（v2。予約フィールド/構造は確保済み）。

- Text ノードの実装（REQ-MOGRAPH-004 本体。GEOMETRY 出力の構造は
  テンプレートに予約済み）
- Expression（Lua）によるパラメータ駆動
- タイムリマップ / 時間伸縮 / 逆再生
- トラックマット
- HDA 的ネットワーク共有・定義の同期更新・ユーザー定義テンプレート
- post-transform Layer Ref、他コンポジションのレイヤー参照、
  文字列による動的参照
- ノードの display flag 的ピン留め
- 複数ネットワークの同時表示・ノードエディタのタブ化
- オーディオリアクティブ（AudioReactive チャネル）

## 実装単位

レビューと切り戻しを容易にするため、以下の単位に分ける。

1. Layer/Document モデル再定義と LayerSource 削除（Phase 1）
2. 動的ポートとパラメータ値ソース（Phase 1）
3. 評価時パラメータ解決と再帰評価・Document-aware evaluator（Phase 1）
4. 殻コンパイラ再設計（Phase 2）
5. Rasterize color 入力とテンプレート定義（Phase 2）
6. Video ノード（ravel-media 橋）（Phase 2）
7. Layer Ref + 循環検出、サブネット（Phase 2）
8. ノードエディタのコンテキスト化（Phase 3）
9. Viewer root comp 評価、プロパティパネル、タイムライン Document 駆動
   （Phase 3）
10. アニメーション UI（Phase 4）
11. 永続化（Phase 4）

各単位で既存テストを通し、モデル変更と UI 変更を同じ差分に詰め込み
すぎない。

## ドキュメント同期

実装と同じ差分または直後の差分で以下を更新する。

- `docs/specifications/data-model.md`（Phase 1: Layer/ネットワーク/
  所有パス、Phase 2: 殻コンパイル）
- `docs/specifications/architecture.md`（Phase 1: evaluator 変更）
- `docs/agent-api-reference.md`（各 Phase の公開 API 変更）
- `docs/ui-impl-status.md`（Phase 3 以降）
- `AGENTS.md`（モデル説明が実装に追随した時点で更新）

## 関連計画との依存

- `playback-foundation-plan.md`（進行中）: PlaybackClock、トランスポート、
  latest-wins の連続評価ループは本モデルでもそのまま利用する。
  同計画の実装単位 3 が配線する `eval.request(graph, node, ctx)` は
  Phase 1 の Document-aware evaluator 変更でシグネチャが変わるため、
  **Phase 3 で PlaybackController の評価要求を root comp 出力・
  Document ベースに更新する**。同計画が繰延した「Composition 出力の
  常時評価」は Phase 3 が本体であり、同計画が繰延した「デコード済み
  メディアフレームの表示」は Phase 2 の Video ノード（ravel-media 橋）
  完了で解禁される。
- 推奨順序: playback 計画の残り（実装単位 2-3）を先に完成させ、
  その後に Phase 1-2、Phase 3 の順で進める。

## 推奨実施範囲

Phase 1 と Phase 2 を第一マイルストーン（コア層で完結し UI 無しで
検証可能）とする。Phase 3 と Phase 4 を第二マイルストーンとする。
第一マイルストーン完了時点で旧モデルのコードパスは削除済みとし、
二重構造の期間を最小化する。
