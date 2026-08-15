# 属性スプレッドシート実装計画（REQ-CORE-010 検査 UI）

> **Status**: Planned — 2026-07-27（単位 1 完了: #302。**単位 2 を書き直し:
> 2026-08-15** — `OVL-2` / `OVL-3` が評価スコープと結果グローバルを先に
> 入れたので、`SelectedGeometry` の新設をやめて相乗りする形にした）

対象: ジオメトリ属性を行×列で目視検査するパネル。関連要件:
REQ-CORE-010（ジオメトリ属性システム）、REQ-UI-013（パネル管理）。

**前提**: `done/free-pane-docking-plan.md` の DOCK-8（カットオーバー）の完了
（旧前提 `panel-placement-plan.md` は同計画に supersede）。新規パネルはどの
プリセットのレイアウトツリーにも無いため、それが直るまで View メニューから
到達できない。

**関連**: `per-instance-modulation-plan.md`。変調グラフは属性名を文字列で
指定する（`field.attribute` の `name`、`field.apply` の `target`）ため、
どんな属性が乗っているかを見る手段が無いと組めない。本計画がそれを担う。

## 問題

属性システム（`ravel-core::geometry`）は 4 ドメイン × 8 型で実装済みで、
`Geometry::summary()` が要素数とドメイン別の属性名/型一覧を返す
`GeometrySummary` も存在する。**が、`ravel-app` / `ravel-ui` のどこからも
呼ばれていない。** 属性を画面で見る手段がゼロ。

`done/geometry-pipeline-ui-plan.md` が「属性検査 UI は属性スプレッドシートまで
保留」と明記して外した分がそのまま残っている。

さらに構造上の制約が 2 つある。

### アプリは評価リクエストを 1 本しか出していない

`ProjectState::build_viewer_request`（`crates/ravel-app/src/project_state.rs:832`）
が組むのは**コンポジション出力 1 ノードだけ**。「選択ノードを評価する」
経路は存在しない。`EvalService` はワーカー 1 本 + `Evaluator` 1 個で、
キューに溜まったリクエストは **latest-wins で捨てる**
（`crates/ravel-core/src/runtime/eval_service.rs:161`）。

同じサービスに 2 系統のリクエストを流すと互いに食い合う。別サービスを
立てると `Evaluator`（＝評価キャッシュと GPU パイプライン）が二重化する。

### Geometry 結果は今のところ黙って捨てられている

`EvalUpdate.result` は `Arc<dyn NodeData>` で汎用だが、
`on_eval_update`（`project_state.rs:932`）は `FrameBuffer` へのダウンキャストに
失敗すると `viewer_blank` に落とす。`Geometry` を評価しても行き先が無い。

## 決定事項（2026-07-27 設計セッション）

### `EvalRequest` を複数ターゲット化する

`node: NodeId` → `nodes: Vec<NodeId>`、`EvalUpdate.result` →
`results: Vec<(NodeId, Result<Arc<dyn NodeData>, EvalError>)>`。
1 リクエストで「コンポジション出力」と「選択ノード」を同時に pull する。

**同じ `Evaluator` を通るのでキャッシュを共有できる**のが要点。ジオメトリ
ノードは通常 rasterize の上流にあるので、コンポジション出力の評価過程で
既に計算済み＝ほぼキャッシュヒットになる。別サービス案の二重キャッシュを
避けられる。

破壊的変更で `project_state.rs` / `playback.rs` / 既存テストに波及するが、
`EvalService` の呼び出し側はアプリ 1 箇所に閉じている。

### 表示対象はノードエディタの選択に追従する

既存のグローバルを読む。新しい選択グローバルは作らない。

**`CanvasSelection` ではなく `SelectedPropertiesTarget` を読む。**
`CanvasSelection.nodes` は `HashSet<NodeId>`（`panels/mod.rs:126`）で
**順序を持たない**ため、「複数選択時の先頭」を定義できない。
一方 `PropertiesTarget::Nodes { network, ids: Vec<NodeId> }`
（`panels/mod.rs:84`）は `Vec` で順序を持ち、Properties パネルが既に
これを唯一の選択対象ソースにしている。同じものを読めば、
スプレッドシートと Properties が常に同じノードを指す。

`ids` の並びがノードエディタ側で決定的かどうかは未確認。単位 2 で
**決定的であることをテストで固定する**。決定的でなければ、書き込み元
（ノードエディタ）を直すのが正しい修正で、読み手側で回避しない。

対象が `Nodes` 以外（`Layer` / `Composition` / `MediaAsset`）のときは
「ジオメトリを出力しない」表示にする。

`NetworkPath` は `EvalRequest.path`（`Vec<PathSegment>`）へ変換して渡す。
レイヤーネットワーク内のノードを、シェル駆動の評価と同じキャッシュキーで
評価するために必須（REQ-LAYER-007/011）。

選択が `Geometry` を出さないノード（rasterize 等）の場合は、パネルは
「このノードはジオメトリを出力しない」を表示し、リクエストの 2 本目を
そもそも送らない。

### v1 は read-only

属性はノードグラフの計算結果なので、セルに書いても次の評価で消える。
書き込みを意味あるものにするには「編集 → 下流に `attribute.set` を挿入」
のような意味付けが要るが、`attribute.set` は全要素ブロードキャストなので
1 セル編集の意図と合わない。v1 では検査に徹する。

### 表示は gpui-component の `DataTable`

`TableDelegate` は `render_td(row_ix, col_ix, ...)` を**可視行だけ**呼ぶ
仮想スクロール実装なので、10 万インスタンスでも行数の上限を切らずに済む。
列固定（`ColumnFixed::Left`）で `index` 列をピン留めし、列幅リサイズと
ソートは組み込みのものを使う。

## 目標構成

```text
SelectedPropertiesTarget ──→ ProjectState.build_viewer_request
                        │  nodes  = [comp_output]          ← 位置規約は不変
                        │  scoped = [(path, graph, node, ctx), ...]
                        ▼
                   EvalService（ワーカー1本 / Evaluator 1個 / キャッシュ共有）
                        │
      results[0] ──→ FrameBuffer ─→ ViewerFrame グローバル ─→ Viewer
      scoped ──────→ 任意の型 ────→ OverlayResults グローバル ─┬→ オーバーレイ
                                    キー = (path, node)        └→ Spreadsheet
```

**この形は `OVL-2`（#429）と `OVL-3`（#437）が入れたもの**で、本計画が
当初描いていた `nodes = [comp_output, selected_node]` /
`SelectedGeometry` グローバルは**採らない**。理由は単位 2 に書いた。

パネル本体:

```text
┌ Attribute Spreadsheet ─────────────────────────────┐
│ [Point] [Primitive] [Instance] [Detail]   1,024 pts │  ← ドメインタブ + 要素数
├─────────────────────────────────────────────────────┤
│ index │ P            │ Cd              │ pscale     │  ← 列 = 属性（型順→名前順）
│     0 │ (0.0, 0.0)   │ (1, 0, 0, 1)    │ 8.0        │
│     1 │ (10.0, 0.0)  │ (1, 0.5, 0, 1)  │ 8.0        │
└─────────────────────────────────────────────────────┘
```

- ドメインタブには要素数を出す。0 要素のドメインはタブを無効化。
- 列順は安定させる（`AttributeSet` は `HashMap` なので `describe()` の
  名前ソートを使い、標準属性を先頭に寄せる）。
- 値の書式は型ごと: F32 は有効数字 4 桁、Vec* はタプル、Color は 4 成分、
  Bool は `true`/`false`、Str はそのまま。

## 実装単位

### 単位 1: `EvalRequest` の複数ターゲット化（`ravel-core`）

- `EvalRequest.node: NodeId` → `nodes: Vec<NodeId>`。
- `EvalUpdate.result` → `results: Vec<(NodeId, Result<Arc<dyn NodeData>, EvalError>)>`。
  ワーカーは `nodes` を順に `evaluate_at` する。1 本目が失敗しても
  2 本目は評価する（片方のエラーで両方止めない）。
- `timings` は従来どおり 1 回分の集約。
- latest-wins コアレスは維持（リクエスト全体が単位）。

**完了条件**

- 2 ノード要求で両方の結果が 1 つの `EvalUpdate` に載るテスト。
- 上流を共有する 2 ノードで、2 本目が**キャッシュヒットする**ことを
  `timings` の非出現で検証するテスト。
- 1 本目が `Err`、2 本目が `Ok` になるテスト。
- 既存の `eval_service` テストを新シグネチャへ移行。

### 単位 2: 選択ノードの評価（`ravel-app`）

> **書き直し（2026-08-15）。** 当初の単位 2 は
> 「`EvalRequest.path` に選択ノードのネットワークを入れ、`SelectedGeometry`
> グローバルを新設する」だったが、**そのまま実装すると動かない。**
> `viewer-overlay-manipulator-plan.md` の `OVL-2`（#429）と `OVL-3`（#437）が
> 同じ問題を先に解いたので、この単位はその上に乗る。旧案は下の
> 「旧案が成り立たない理由」に残す。

#### やること

**既にある機構に相乗りする。新しいグローバルも新しい評価経路も作らない。**

- `EvalRequest.scoped: Vec<ScopedTarget>` に、選択ノードのターゲットを足す。
  1 ターゲットが `(path, graph, node, ctx)` を持つので、レイヤーネットワーク
  内部のノードをレイヤーローカル時間で評価できる（`OVL-3` が入れた形）
- 結果は `OverlayResults`
  （`crates/ravel-app/src/panels/viewer/overlay.rs`）から読む。キーは
  `OverlayResultKey = (Vec<PathSegment>, NodeId)`
- **この単位の本体は「オーバーレイ以外の消費者がターゲットを宣言する経路」**
  である。今は `ViewerOverlay::eval_target` →
  `OverlayRegistry::eval_targets` に閉じており、スプレッドシートは
  オーバーレイではない。宣言元を一般化する
  （`OverlayResults` / `OverlayResultKey` は `ravel-app` 内なので、
  クレートを跨ぐ設計変更にはならない）
- `PropertiesTarget::Nodes` の `ids` の並びが**決定的であることをテストで
  固定する**（下記「表示対象はノードエディタの選択に追従する」の未確認事項。
  ここは旧案のまま有効）
- 対象が `Nodes` 以外（`Layer` / `Composition` / `MediaAsset`）のとき、
  および `Geometry` を出さないノードのときの表示（旧案のまま有効）

#### 旧案が成り立たない理由

- **`EvalRequest` は 1 リクエスト = 1 グラフ + 1 パス。**
  viewer の要求は `path: Vec::new()`（root スコープ）でコンプ出力を
  target 0 に置く。選択ノードのネットワークをリクエストの `path` に入れると
  **コンプ出力の評価が壊れる**。`OVL-2` の最初の実装がこの穴を踏み、
  レビューで「恒偽のスコープ判定」として発見された
- **`SelectedGeometry` を `Option<(NodeId, Arc<Geometry>)>` にすると
  `OVL-2` で潰したバグを再導入する。** `NodeId` 単独は同一性ではない —
  合成ノードの ID は `comp_id << 32 | layer_id << 8 | role`
  （`crates/ravel-core/src/composition/compile.rs`）なので、`comp_id == 0`
  では通常のノード ID の範囲に落ちて衝突し、**別ノードの結果を表示する**。
  `OverlayResultKey` がパスを含むのはこのため
- **旧完了条件 3 件は既に満たされ、テストで固定済み**（`OVL-2` #429）:
  `results[0]` はコンプ出力のままなので Viewer が乗っ取られない、
  target の失敗は `Err` としてスロットを保つので Viewer が生き残る、
  フレームが `Blank` / `Error` のときは結果を公開しない
- **`build_viewer_request` の改称は不要。** 複数ターゲットは `scoped` が
  担い、`results` の位置規約（`results[0]` = コンプ出力）は
  `ViewerUpdate::from_eval` が依存しているので変えない

#### 完了条件

- ジオメトリノードを選択 → その結果が `OverlayResults` から読め、
  同時に `ViewerFrame` も従来どおり更新されるテスト
- 選択解除で結果が消えるテスト
- **別ネットワーク / 別コンプの同じ `NodeId` を持つノードの結果を
  読まないテスト**（`OVL-2` の衝突テストと同じ形）
- `PropertiesTarget::Nodes` の `ids` の並びが決定的であることのテスト。
  決定的でなければ**書き込み元（ノードエディタ）を直す**。読み手側で
  回避しない
- 選択ノードの評価が失敗しても Viewer が生き残るテスト
  （`OVL-2` が固定済みだが、この単位が足す経路でも成り立つことを確認する）

### 単位 3: パネル本体（`ravel-app` / `ravel-ui`）

- `PanelKind::AttributeSpreadsheet` を追加。`PanelKind::MediaBin` の
  実際の配線を辿って洗い出した登録先は以下。**`panels/mod.rs` は
  `ravel-ui` 側と `ravel-app` 側の 2 つある**ので取り違えない。

  | ファイル | 登録内容 |
  |---|---|
  | `crates/ravel-ui/src/panel.rs` | enum バリアント / `ALL`（要素数 16 → 17）/ `panel_id` / `label_key` |
  | `crates/ravel-ui/src/command.rs` | `CommandId` バリアント / コマンド名テーブル / メニューキー |
  | `crates/ravel-ui/src/menu.rs` | View メニュー項目 |
  | `crates/ravel-ui/src/shell.rs` | `handle_command` のトグル分岐 |
  | `crates/ravel-ui/src/panels/mod.rs` | ヘッドレスパネルモジュール宣言 |
  | `crates/ravel-app/src/panels/mod.rs` | モジュール宣言 + パネル生成分岐 |
  | `crates/ravel-app/src/workspace.rs` | パネル生成分岐 + トグル対象マップ |
  | `crates/ravel-app/src/assets.rs` | `RavelIcon` バリアント + アイコンパス |
  | `assets/icons/` | アイコン SVG |
  | `assets/locales/{en,ja}.toml` | パネル名とメニューラベル |

  DOCK-2（既定スロット挿入）完了後なので `crates/ravel-ui/src/preset.rs`
  の編集は不要（MediaBin は #180 でそれを必要とした）。
- `panels/attribute_spreadsheet.rs`: `TableDelegate` 実装。
  ドメインタブ、列構築、型別セル書式、`index` 列の固定。
- 空状態 3 種を明示的に出し分ける: 選択なし / 選択ノードがジオメトリを
  出力しない / ジオメトリが空。

**完了条件**

- 列構築と値書式のヘッドレステスト（`TableDelegate` はウィンドウ無しで
  `columns_count` / `rows_count` / 列定義を検証できる）。
- ドメインタブ切替で行数と列が入れ替わるテスト。
- 空状態 3 種のテスト。

### 単位 4: 実機確認と文書更新

- 実機（cliclick）: `scatter.grid` を選択して instance ドメインに
  `index` / `P` / `rot` / `scale` が並ぶこと。1 万インスタンスで
  スクロールが詰まらないこと。
- `docs/specifications/ui-spec.md`: パネル一覧に追記。
- `docs/requirements/REQ-CORE.md`: REQ-CORE-010 に検査 UI の記述が
  無いので触らない。代わりに REQ-UI 側の記述を確認して必要なら追記。
- `docs/implementation/README.md`: 本計画と
  `per-instance-modulation-plan.md` の表記を確認・更新。

**完了条件**

- `mise run check` が通る。
- 実機で 1 万インスタンスのスクロールが破綻しない。

## 検証

- ユニット: `cargo test -p ravel-core -p ravel-ui -p ravel-app`
- 単位 1 のキャッシュ共有は `timings` で機械的に検証する（体感ではなく）。
- パフォーマンス: 仮想スクロール前提なので行数は上限を切らない。
  代わりに**選択ノードの評価が Viewer のフレームレートを落とさないこと**を
  `perf-baseline.md` の手順で 1 回測る。キャッシュヒットしない構成
  （rasterize の上流に無い孤立ジオメトリノードを選択）が最悪ケース。

## 非対象

- **セル編集**。上記「決定事項」の理由により v1 では持たない。
- **属性の統計表示**（min/max/平均）。列ヘッダに出したくなるが、
  全要素走査が毎フレーム走るので別途キャッシュ設計が要る。
- **複数選択の同時表示**。`PropertiesTarget::Nodes` の `ids` 先頭のみ。
- **`instance_source` の中身の表示**。インスタンスソースは入れ子の
  `Geometry` なので、掘るなら階層 UI になる。ドメインタブに収まらない。
- **レイアウトの永続化**。`done/free-pane-docking-plan.md`（DOCK-9）の担当。
- **プリミティブドメインのトポロジ表示**（`Primitive::Path` の
  `verts` 範囲）。属性列だけを出し、プリミティブ構造は出さない。
