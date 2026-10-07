# 編集操作の層 実装計画

> **Status**: Planned — 2026-10-07

対象: 「パラメータを設定」「レイヤーを追加」「ノードをつなぐ」を、**引数を持つ型付きの
操作**として定義し、`Document` → `Document` で適用する層。企画書
`ai-assistant-mcp-plan.md` の `AI-2` を、着手できる単位（`AI-2a`〜`AI-2d`）に割る。
要件は `REQ-PLUGIN-006`（操作自動化）と `REQ-PLUGIN-007`（MCP）。複数クレートに
またがるので、`AGENTS.md` の Design gate に従いコードの前に書く。

行番号は 2026-10-07 時点の `main`（`8361957b`）で数えた。計画書が古びたら、
実装を正とする。

## 問題

文書を編集するコードは、UI の各所に散らばっている。

- 「レイヤーを足す」「接続する」「パラメータを書く」を呼ぶ側が、そのつど
  `Document` を組み立て、`ProjectState::commit_document` に渡す
  （`crates/ravel-app/src/project_state.rs:1245`）。ravel-app 内でこの呼び出しを
  含む行は src に 161 行、tests に 27 行ある（`rg -c "commit_document\("`）
- 組み立ての**純粋な部分**は半分だけ切り出されている。`ravel-ui::document` に
  `add_layer` / `remove_layers` / `replace_network` などがある。ところが
  **接続・ノード削除・パラメータ書き込み**は `ravel-app`（GPUI のクレート）の
  中にあり、ヘッドレスから呼べない
- 引数の検証がまちまちで、失敗の返し方が呼び出し側ごとに違う。切り出された関数は
  `Option<Document>`（理由が無い）、パネル内のものは `tracing::warn!` して黙って
  捨てる（`node_editor.rs:2145`）

MCP（`AI-3`〜`AI-5`）と自動化（`REQ-PLUGIN-006`）は、これらを**GUI なしで、理由付きの
エラーで**呼べなければならない。UI と別に実装すれば、同じ編集の経路が 2 本になり、
片方だけが直される。

## 現状の調査結果

### 編集の経路

- `ProjectState::commit_document(doc, hint, cx)`（`project_state.rs:1245`）は
  `store.commit(doc)` を呼んで評価を再要求するだけ。`apply_document`（:1232）は
  ジェスチャ中用で undo 段を作らない
- 呼び出し元の内訳（`rg -c "commit_document\("`、行数）: `project_state.rs` 53 /
  `timeline.rs` 30 / `properties.rs` 28 / `viewer.rs` 18 / `node_editor.rs` 17 /
  `outliner.rs` 11 / `media_bin.rs` 4。ほか tests に 27 行
- `CommandId`（`ravel-ui/src/command.rs`）は引数を持たない UI コマンドで、
  「レイヤー 3 の不透明度を 0.5 に」を運ばない。この層は `CommandId` の下に
  置く（コマンドがこの層の操作を呼ぶ側になる）

### すでに純粋関数になっているもの（`crates/ravel-ui/src/document.rs`、2250 行）

| 関数 | 行 | 戻り値 |
|---|---|---|
| `update_composition` / `update_layer` | 243 / 256 | `Option<Document>` |
| `add_layer` / `duplicate_layer` / `remove_layer` | 273 / 280 / 293 | `Option<Document>` |
| `update_layers` / `remove_layers` / `duplicate_layers` | 301 / 320 / 342 | `Option<Document>` |
| `split_layer(s)` / `reorder_layer` | 392, 425 / 459 | `Option<Document>` |
| `add_layer_from_template` | 506 | `Result<Option<(Document, LayerId)>, TemplateError>` |
| `add_media_layer(s)` | 570 / 640 | 同上系 |
| `add_composition` / `duplicate_composition` / `remove_composition` | 875 / 893 / 927 | `(Document, CompId)` / `Option<_>` |
| `resolve_network` / `replace_network` | 942 / 962 | `Option<_>` |

- 依存は `ravel-core` のみ（`ravel-ui/Cargo.toml`）で、GPUI を引かない。ただし
  `ravel-cli` は `ravel-ui` に**直接依存できない**（`.agents/rules/rust.md`）。
  `ravel-project` が `ravel-ui` に依存するので推移的には届くが、それを足場に
  ヘッドレス側が UI クレートの API を呼ぶ形は規約の趣旨に反する
- `NetworkPath { comp, layer, subnets }`（`document.rs:33`）がネットワークの
  アドレスで、`ravel-ui` にある。名前が出る行は 14 ファイルで計 225 行
  （`rg -c "NetworkPath"`）
- 戻り値が `Option` なので、失敗の理由（コンプが無いのか、レイヤーが無いのか）が
  呼び出し側に渡らない

### まだ UI クレートの中にあるもの

- **接続**: `connect_edge_and_update_variadic_inputs`
  （`ravel-app/src/panels/node_editor.rs:353`）。既存のエッジを置換し、可変入力の
  空きスロットを選び、末尾に新スロットを足す。**`Graph::add_edge`
  （`ravel-core/src/graph.rs:1268`）はポート番号も型も検証しない**（存在・重複・
  循環だけ）。型の互換は `first_compatible_port`（`node_editor.rs:269`）のような
  UI 側の関数が見ている
- **ノード削除**: `remove_node_and_compact`（`node_editor.rs:332`）。後段の可変入力を詰める
- **パラメータ書き込み**: `ravel-app/src/panels/param_edit.rs`（479 行）の
  `edited_param_value`（:153）が「アニメ済みパラメータはキーを打つ・定数は値を
  置換・範囲でクランプ」を決める。`ravel_ui::keyframes::set_curve_value` に
  依存する。その結果を `dependent_param_updates` /
  `dependent_port_updates`（`ravel-core/src/registry/builtin.rs:490` / `:529`）で
  従属する値・出力ポート型へ広げ、`Graph::set_params_and_output_types`
  （`graph.rs:1419`）で 1 回に書く（`node_editor.rs:2072-2150`）
- 他にも `Graph::set_params`（`graph.rs:1395`）を直接呼ぶ箇所が
  `properties.rs`・`project_state.rs`・`exposed/apply.rs` にある。
  `set_params` の挙動（存在しないキーを**無視**する、`graph.rs:1395` 周辺の
  ドキュメント）は、この層の「未知のキーはエラー」と食い違う

### undo の単位

- `DocumentStore`（`ravel-ui/src/document.rs:98`）は `live: Document` と
  `UndoStack<Document>`。`commit` が履歴に 1 段積み（:127）、`apply` は積まない
  （:121）。最大 200 段（:110）。履歴は構造共有のスナップショット
  （`ravel-core/src/undo/stack.rs`）で、`Document` は `im` を使うので複製が安い
- **グループ化の仕組みは無い。** 「複数の編集を 1 段に」は、複数の変更を
  1 つの `Document` に畳んでから `commit` を 1 回呼ぶことで実現している
  （`update_layers` の説明、`document.rs:301` 付近）
- `ravel-core::undo::GraphMutation`（`undo/mutation.rs`）はグラフ単位の
  ジャーナル用の変更（`AddNode` / `RemoveNode` / `UpdateNodeMetadata` /
  `AddEdge` / `RemoveEdge`）。**ジャーナルの語彙であり、この層の操作ではない**:
  グラフ単位で、コンプ・レイヤー・パラメータを持たず、可変入力の整理も
  しない。この層を `GraphMutation` の上に作ると、レイヤー系の操作が乗らない

### ノード作成と ID

- ノードは `NodeRegistry::create_node(type_key, id) -> Option<Node>`
  （`ravel-core/src/registry/mod.rs:982`）でテンプレートから作る。未登録の
  type_key は `None`。subnet / iterate は内側のグラフも作り、**追加の ID を
  採番する**（`NodeTemplate::create_node`、`:941`）
- ID は `NodeId::next()` / `EdgeId::next()` / `LayerId::next()` / `CompId::next()`
  （`ravel-core/src/id.rs`）のグローバルなアトミックカウンタ。読み込み時は
  `Document::advance_id_counters()`（`composition/mod.rs:2036`）が呼ばれ、
  `ProjectFile::load` がそれを呼ぶ（`ravel-project/src/lib.rs:383`）。
  ヘッドレスが読み込んだ文書へ足す限り、新 ID は衝突しない
- 一意性: `Graph::add_node` は**そのグラフ内**の重複しか見ない
  （`graph.rs:1220`）。ドキュメント全体での一意性は
  `Document::validate`（`composition/mod.rs:2060`）の `DuplicateNodeId` が見る。
  `validate` は構造だけで、`layer.ref` の宙吊りはあえて見ない（同関数の説明）

### ショーケース生成器が手で組み立てた操作

`.worktrees/node-showcase/crates/ravel-cli/examples/showcase_project.rs`（1204 行、
読むだけ）は、まさにこの層の最初の操作集合を手で書いている。

| 生成器の関数 | 行 | 対応する操作 |
|---|---|---|
| `Net::add`（レジストリからノードを作り、既定値の一部を上書き、位置を置く） | :114 | `AddNode { type_key, position, params }`。**未知のキーで panic** = この層の「未知のパラメータキーはエラー」 |
| `Net::wire_at` / `wire`（ポート番号 / ポート名で接続） | :134 / :149 | `Connect`。名前でも番号でも指せる必要がある |
| `Net::grow`（可変入力を 1 つ開く） | :166 | `Connect` の副作用に含める（UI の `connect_edge_and_update_variadic_inputs` と同じ） |
| `Net::finish`（`net.in` / `net.out` を作って繋ぐ） | :175 | 新規レイヤーのテンプレート（`AddLayerFromTemplate`）が持つべきもの |
| `document`（コンプを作り、レイヤーを足す） | :1131 | `AddComposition` / `AddLayer` |
| `place`（レイヤーの殻の変換を設定） | :1115 | **この計画の最初の集合に入れない**（殻のプロパティ。後続） |
| `fill_body`（iterate の内側グラフに Transform を挟む） | :978 | `NetworkPath.subnets` を使う `AddNode` / `RemoveEdge` / `Connect` |

読み取れること: (a) 操作は**ネットワークのパス**を引数に持つ必要がある
（`subnets` を含む）。(b) ID は呼び出し側が先に知れない（`NodeId::next()` を
操作の内部で採番し、結果として返す）。(c) 登録済みの type_key とパラメータキーの
検証は、層の責任にしないと呼び出し側が毎回書く。

## 決定事項

### 1. 置き場所: `ravel-core` の新モジュール `edit`

| 案 | 判断 | 理由 |
|---|---|---|
| **`ravel-core::edit`** | **採用** | `ravel-cli` が直接呼べる。`Document` / `Graph` / `NodeRegistry` / `ID` はすでにここにあり、`exposed/apply.rs`（`Document` → `Document` の純粋関数）という前例がある |
| `ravel-ui::document` に足す | 不採用 | `ravel-cli` は `ravel-ui` に依存できない（`rust.md`） |
| 新クレート `ravel-edit` | 不採用 | 依存辺が 1 本増えるだけで、`ravel-core` の外に出す理由が無い。必要になってから切り出せる |

- `ravel-ui::document` の純粋関数のうち、この層の操作が使うものは `ravel-core::edit`
  へ**移し**、`ravel-ui::document` は同名で `pub use` する。既存の呼び出し元
  （`ravel_ui::document::` を含む行が ravel-app に 294 行ある）を一括で
  書き換えない
- `NetworkPath` も `ravel-core::edit` へ移し、`ravel-ui` が再エクスポートする
  （`document.rs:33`。`ravel-core::network::NetworkContext` に変換する
  `context()` は、すでに「core は `NetworkPath` を見られない」ために UI 側で
  折り畳んでいる — 同じ型を core に置けばこの折り畳みは不要になるが、
  **この計画では消さない**。移動だけ）
- `ravel_ui::keyframes::set_curve_value` はパラメータ書き込み（`AI-2c`）が使う。
  `ravel-ui` は `ravel-core` のみに依存するので、これも `ravel-core` へ移して
  再エクスポートする

### 2. 操作の型: `enum EditOp`（`Serialize` / `Deserialize`）

- **enum を採る。** 操作の集合は閉じている。`match` の網羅性をコンパイラが守る
  （新しい操作を足して処理を忘れるとコンパイルが通らない）。trait + `dyn` は
  直列化に `typetag` のような依存が要り、新規の production 依存は足さない
- `Serialize` / `Deserialize` を**要件にする。** MCP の JSON 引数になり、将来の
  自動化スクリプトの入力にもなる。`serde` は `ravel-core` が既に持つ。JSON 文字列の
  読み書きは呼び出し側（`ravel-cli` は `serde_json` を持つ）で、`ravel-core` に
  `serde_json` は足さない
- 引数の値は `ParameterValue` そのままでなく、JSON で書ける**入力型**
  `ParamInput`（`Float` / `Int` / `Bool` / `String` / `Floats(Vec<f32>)` …）にする。
  `ParameterValue` は `AnimationChannel` を含む内部表現で、JSON 化の安定性が
  **未確認**（`ravel-core` に `serde_json` での往復テストは無い）。入力型は
  `ravel_ui::properties::PropertyValue`（現 UI の値）に近い形で、既存の
  書き込み規則（下記 4）がそのまま使える

### 3. 適用とエラー

```text
pub fn apply(doc: &Document, op: &EditOp, cx: &EditContext) -> Result<Applied, EditError>
pub fn apply_batch(doc: &Document, ops: &[EditOp], cx: &EditContext) -> Result<Applied, BatchError>
```

- `Applied { document: Document, created: Vec<Created> }`。`Created` は採番した
  `NodeId` / `EdgeId` / `LayerId` / `CompId`。呼び出し側は結果から ID を知る
- `EditContext { registry: &NodeRegistry, local_frame: Option<u64> }`。レジストリは
  ノード作成とパラメータの範囲、`local_frame` はアニメ済みパラメータへの書き込み
  （下記 4）が使う
- **文書を変えずにエラー**は型で保証する: `apply` は `&Document` を取り新しい
  `Document` を返す。エラー時は呼び出し側の文書に何も起きない。成功時も
  `Document::validate()` を通ってから返す（`DuplicateNodeId` など、構造の破れを
  その場で見つける）。**1 操作ごとの全文書走査のコストは未計測**。計測して
  重ければ、触ったグラフだけを検査する形に狭める（`AI-2d` の完了条件）
- `EditError`（`thiserror`）は呼び出し側が分岐できる粒度で持つ:
  `CompositionNotFound` / `LayerNotFound` / `LayerLocked` / `NodeNotFound` /
  `EdgeNotFound` / `UnknownNodeType` / `UnknownParameter` /
  `ParameterTypeMismatch` / `PortNotFound` / `PortTypeMismatch` /
  `AnimatedParameter`（4 を参照）/ `Graph(GraphError)` /
  `InvalidDocument(DocumentValidationError)`
- 各バリアントに**翻訳されない安定 ID**（`fn code(&self) -> &'static str`、
  `"layer-not-found"` など）を持たせる。`ravel-cli` が警告と失敗に付けている
  `media-offline` / `binding-issue` と同じ流儀で、MCP のエラーがそのまま使う
- `apply_batch` は**全部成功か、全部不成功**。内部で作業用 `Document` に順に
  適用し、失敗した操作の添字を `BatchError` で返す。呼び出し側は成功した
  `Applied` を `commit_document` に 1 回渡す = **undo 1 段**

### 4. パラメータ書き込みの規則

- 現 UI の規則（`param_edit.rs:153`）を**そのまま移す**: 定数は値を置換する、
  アニメ済み（キーフレーム）パラメータは `local_frame` にキーを打つ、数値は
  レジストリの `ParamRange` でクランプする、`Float` を `Int` のチャンネルへ書く
  ような型違いは拒否する
- 従属する値と出力ポート型は、UI と同じ `dependent_param_updates` /
  `dependent_port_updates` で広げ、`set_params_and_output_types` で 1 回に書く
- **UI と食い違う点（2026-10-07 決定。末尾の「決定済みの未決事項」1）**: キーフレーム済みで
  `local_frame` が無いとき、現 UI は**定数で潰す**（`param_edit.rs:10` の
  `edited_channel` の `None` 腕）。UI にはプレイヘッドが必ずあるので起きないが、
  ヘッドレスの呼び出しでは「アニメを黙って消す」ことになる。この層では
  `AnimatedParameter` エラーにして、呼び出し側に「キーを打つフレーム」か
  「アニメを捨てる明示の操作」を選ばせる
- 未知のキーは `UnknownParameter` エラー。`Graph::set_params` の「無視する」
  （`graph.rs:1395` の説明）には**依存しない**: エラーにする検査を層で先に行う

### 5. 最初に入れる操作の集合

| 操作 | 引数（要点） | 破壊的 | 既存の関数 |
|---|---|---|---|
| `AddComposition` | `CompositionSettings` | — | `add_composition`（`document.rs:875`） |
| `RemoveComposition` | `comp` | はい | `remove_composition`（:927） |
| `AddLayer` | `comp`、テンプレートキー | — | `add_layer_from_template`（:506） |
| `RemoveLayer` | `comp`、`layers` | はい | `remove_layers`（:320。ロック済みは `LayerLocked` エラーにする: 現状は黙って飛ばす） |
| `AddNode` | `path`、`type_key`、`position?`、`params?` | — | `NodeRegistry::create_node` + `replace_network` |
| `RemoveNode` | `path`、`node` | はい | `remove_node_and_compact`（`node_editor.rs:332`） |
| `Connect` | `path`、出力側 / 入力側（ノード + ポート名または番号） | — | `connect_edge_and_update_variadic_inputs`（`node_editor.rs:353`） |
| `Disconnect` | `path`、`edge` | はい（接続の削除は undo で戻せるので**確認不要**。分類は `AI-5` で決める） | `remove_edge_and_compact` |
| `SetParameter` | `path`、`node`、`key`、`ParamInput` | — | `edited_param_value` + `set_params_and_output_types` |

- `EditOp::is_destructive()` を持つ。確認ダイアログそのものは `AI-5`（アプリ側）で、
  この層は**分類を返すだけ**。企画書の「確認の対象は削除と上書きだけ」に沿う
- 殻のプロパティ（変換・不透明度・時間配置）、キーフレーム操作、ノードの移動、
  レイヤーの並べ替え・複製・分割は**この計画の対象外**（下記）。ショーケースの
  `place`（:1115）が欲しがる `SetLayerShell` は、`AI-3` の着手時に必要なら
  後続の単位として足す

### 6. 既存 UI を移す方針

「経路が 2 本にならない」を、**移した関数を元の場所から消す**ことで守る。
新しい層を作って旧実装を残すと、まさに 2 本になる。

- `AI-2a`: `ProjectState::add_layer_from_template`（`project_state.rs:1761`）と
  `create_composition`（:2025）が `edit::apply` を呼ぶ
- `AI-2b`: ノードエディタの接続・ノード削除（`node_editor.rs:332` / `:353` の
  ヘルパを削除し、`edit` の実装へ置換）
- `AI-2c`: `NodeEditorPanel::apply_property_change`（`node_editor.rs:2072`）と
  `param_edit.rs` の書き込み規則（`edited_param_value` ほか）を `edit` へ移す。
  Properties パネルが同じ関数を通る

### 7. 複数操作を 1 undo 段にまとめる

新しい仕組みは作らない。`apply_batch` が 1 つの `Document` を返し、呼び出し側が
`commit_document` を 1 回呼ぶ（上記 3）。`DocumentStore` に「グループ開始 / 終了」を
足すと、グループの閉じ忘れという新しい状態が生まれる。ジェスチャ中の更新
（`apply_document`）は引き続き UI だけが使い、この層は関与しない。

### 8. 同一バッチ内で作ったものは別名（`as`）で参照する

ID は操作の中で採番され、結果として返る（`Created`）。それだけだと、同じバッチで
作ったノードを後続の操作が指せず、「ノードを作って、そのままつなぐ」を 1 回の
呼び出し（undo 1 段）にまとめられない。そこで別名を**最初から**持つ
（2026-10-07 決定。後から足すと `EditOp` の直列化形式が変わるため）。

- 作る操作（`AddComposition` / `AddLayer` / `AddNode`）は省略可能な `as`
  （例 `"as": "blur"`）を取る
- 対象を指す引数は ID 直書きでなく参照型（`CompRef` / `LayerRef` / `NodeRef` =
  `Id(..)` か `Alias(String)`）にする。JSON では数値なら ID、文字列なら別名
- 別名の有効範囲は**1 バッチ**。バッチをまたいで残さない（永続化しない）
- 同じバッチ内での別名の重複は `DuplicateAlias`、未定義の別名は `UnknownAlias`。
  どちらもバッチ全体が不成功（文書は不変）
- `Applied` は別名 → 採番された ID の対応を返す。`AI-3` の頭なし組み立ても、
  1 操作ずつ送る使い方と、組み立てを 1 バッチで送る使い方の両方ができる

## 目標構成

```text
 UI（ravel-app: パネル、コマンド）        ravel-cli（AI-3: 頭なし）
 MCP ライブ口（AI-4: アプリ内）           自動化スクリプト（REQ-PLUGIN-006）
              │                                  │
              └────────────┬─────────────────────┘
                           ▼
              ravel_core::edit::apply / apply_batch
              （EditOp + EditContext → Applied | EditError）
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
        Document /    NodeRegistry   Document::validate
        Composition   （テンプレート）  （構造の破れを検出）
        / Graph
                           │
                           ▼
        呼び出し側が commit する（ProjectState::commit_document = undo 1 段）
```

- `ravel-core::edit` は `gpui` も `ravel-ui` も知らない
- 呼び出し側が commit を持つので、`edit` は undo にも評価にも触れない

## 実装単位

```text
AI-2a ──▶ AI-2b ──▶ AI-2c ──▶ AI-2d
```

直列に並べる理由: どれも `ravel-core::edit` の同じファイルを触り、`AI-2a` が
エラー型と `apply` の骨格を決める。並行に進めると `EditOp` / `EditError` で
衝突する。

### `AI-2a` 骨格、コンポジションとレイヤーの操作

- `crates/ravel-core/src/edit/`（`mod.rs`、`error.rs`、`ops.rs`）を新設。
  `EditOp` / `EditContext` / `Applied` / `Created` / `EditError`（`code()` 付き）/
  `apply` / `apply_batch`
- `AddComposition` / `RemoveComposition` / `AddLayer` / `RemoveLayer`
- `NetworkPath` と、これらが使う `ravel-ui::document` の純粋関数を移し、
  `ravel-ui` が再エクスポートする
- `ProjectState::add_layer_from_template` と `create_composition` を `edit::apply`
  経由にする

**完了条件**

- 各操作の適用が期待どおりの `Document` を返し、`validate()` が通る
- 不正な引数（存在しないコンプ・レイヤー、未知のテンプレート、ロック済みレイヤーの
  削除）が `Err` を返し、**渡した `Document` が変わらない**（`==` で確認）
- `apply_batch` が途中で失敗したとき、文書が変わらず、失敗した添字が返る
- 別名: 同じバッチで `AddComposition { as }` → `AddLayer { comp: Alias }` が通り、
  `Applied` が別名と ID の対応を返す。重複は `DuplicateAlias`、未定義は
  `UnknownAlias` で、どちらも文書は不変
- `EditOp` を JSON（`serde_json`、テスト限定の dev-dependency でなければ
  `ravel-cli` 側のテスト）に往復させて同値になる
- `ravel-ui::document` の再エクスポート経由で既存の呼び出し元が**変更なしで**
  コンパイルでき、`mise run check` が通る
- `ProjectState` の 2 つの入口が `edit::apply` を呼び、旧実装が残っていない
  （`rg "add_layer_from_template\("` で呼び出しが 1 系統）

### `AI-2b` ネットワークの操作

- `AddNode` / `RemoveNode` / `Connect` / `Disconnect`
- 接続は**ポート番号と型の検証をこの層で行う**（`Graph::add_edge` は見ない。
  `graph.rs:1268`）。ポートは名前でも番号でも指せる。可変入力の空きスロット選択と
  末尾の拡張、既存エッジの置換は `connect_edge_and_update_variadic_inputs`
  （`node_editor.rs:353`）と同じ結果にする
- `NetworkPath.subnets` を通した内側のグラフの編集（`replace_network` が
  サブネットのピンを同期する: `document.rs:962`）
- ノードエディタの接続・ノード削除を `edit::apply` 経由にし、
  `node_editor.rs` のヘルパ（`remove_node_and_compact` / `connect_edge_and_update_variadic_inputs` /
  `remove_edge_and_compact` / `compact_empty_variadic_inputs`）を `edit` へ移す

**完了条件**

- 型の合わない接続・範囲外のポート・存在しないノード・循環が `Err` で、文書は不変
- 可変入力: 接続で空きスロットが 1 つに保たれ、切断で詰まる（既存の
  `node_editor.rs` のテストを、移した先で**同じ内容のまま**通す）
- サブネット内の `AddNode` / `Connect` がピン同期を含めて 1 つの `Document` になる
- 未登録の `type_key` が `UnknownNodeType`。subnet / iterate の内側のノード ID も
  含めて `Document::validate()` が通る（ドキュメント全体で一意）
- ショーケース生成器の `fill_body`（iterate の内側に Transform を挟む）を、
  `AddNode` + `Disconnect` + `Connect` の列で再現するテスト
- ノードエディタのヘルパが残っていない（`rg "connect_edge_and_update_variadic_inputs"`
  が `edit` の中だけ）

### `AI-2c` パラメータの操作

- `SetParameter` と `ParamInput`
- `param_edit.rs` の書き込み規則（`edited_param_value` / `edited_float_param` /
  `edited_int_param` / `edited_string_param` / `edited_vector_param`）を `edit` へ移す。
  `ravel_ui::keyframes::set_curve_value` も `ravel-core` へ移し、再エクスポートする
- 未知のキーは `UnknownParameter`、型違いは `ParameterTypeMismatch`、
  アニメ済みで `local_frame` が無いときは `AnimatedParameter`（決定事項 4。
  キーを黙って消さない）
- `NodeEditorPanel::apply_property_change`（`node_editor.rs:2072`）を `SetParameter` に
  置換。Properties パネルの書き込みも同じ関数を通る

**完了条件**

- 現 UI の書き込み規則のテスト（`param_edit.rs` のもの）を、移した先で**弱めず**
  通す
- 範囲外の値がクランプされる。未知のキー・型違い・存在しないノードが `Err` で
  文書は不変
- アニメ済みのパラメータに `local_frame` を渡すとキーが打たれ、既存のキーが
  残る。渡さないと `AnimatedParameter` で文書は不変
- `attribute.set` の `type` を変えると `value` の形が追従し、`layer.ref` の
  出力ポート型が追従する（`dependent_*_updates` を通っていること）。
  1 回の `apply` が 1 つの `Document` を返す
- `ParamInput` が JSON で往復する
- `param_edit.rs` の書き込み関数が残っていない

### `AI-2d` バッチ、ホスト側の入口、実機相当の検証、文書

- ホスト側の薄い入口: `ProjectState::apply_edit(ops, cx)`（`ravel-app`）。
  `apply_batch` の結果を `commit_document` 1 回で確定する。`AI-4` のライブ口が
  呼ぶ入口で、ここで **1 バッチ = undo 1 段**を固定する
- 1 操作あたりの `Document::validate()` の計測（大きなプロジェクト = ショーケース
  相当）。重ければ検査を触ったグラフに限る
- ショーケースの手組みの一部（例: 1 コンプ・3 レイヤー・各レイヤー 2〜3 ノード）を
  `EditOp` の列で組み立て、`ProjectFile::save` → `load` → `validate` を通る
  テストを `ravel-project` に置く（`ravel-core` は `ravel-project` に依存できない。
  `AI-3` の統合テストの下地になる）
- `docs/dev/` に「編集操作を足す」のチェックリスト（`add-command.md` と
  `docs/dev/README.md` の索引に載せる）。`docs/agent-api-reference.md` に
  `ravel-core::edit` を追記
- `.agents/rules/gpui.md` の Command 経路の節に 1 行: 文書の編集は `edit` を通す

**完了条件**

- `apply_edit` が 1 バッチで undo 段を 1 つだけ増やし、`Cmd+Z` 相当の `undo` で
  元の文書に戻る（`ravel-app` の `gpui::test`）
- 失敗したバッチで undo 段が増えず、文書が変わらない
- 手組みの代表例の保存・読み込み・検証のテスト
- `validate()` の計測結果が計画書（この文書）の「検証」に記録されている
- `mise run docs:check` が通る

## 検証

- 各単位で `mise run check`（fmt、pattern lint、clippy、workspace テスト）
- 「文書を変えずにエラー」は、全操作について `before == after`（`Document: PartialEq`）
  を確認するテストで押さえる。空の `for` に落ちない（操作の一覧が空でないことを
  先に `assert!` する）
- 既存テストは**弱めない**。移した関数のテストは移動先で同じ内容のまま通す。
  もし既存のテストがバグを固定していたら、直さずに報告する
- 移した関数の旧実装が残っていないことを `rg` で確認する（上記の各完了条件）。
  十分に安定したら `scripts/lint-patterns.sh` に「`ravel-app` で `Graph::add_edge`
  を直接呼ばない」類の規則を足すかを判断する（足すなら `.agents/rules/` に理由を書く）
- GPU・FFmpeg は要らない。`ravel-core::edit` のテストはヘッドレスで走る

## 非対象

- 殻のプロパティ（変換・不透明度・時間配置・ブレンド）、キーフレームの追加・削除、
  ノードの移動、レイヤーの並べ替え・複製・分割・メディアの取り込み
  （必要になった時点で後続の単位として足す）
- UI のジェスチャ中の更新（`apply_document`）。ドラッグ中の毎フレームを `EditOp` に
  するのは過剰で、確定時に 1 操作として commit する
- 文書の読み書き（`ProjectFile`）。保存先の制限は `AI-3`
- 確認ダイアログ（`AI-5`）。この層は `is_destructive()` を返すだけ
- 自動化スクリプトの言語（`REQ-PLUGIN-006`）。この層は言語に依存しない
- ジャーナル（`GraphMutation`）の置き換え。別の目的（クラッシュ復旧）で、
  この層は載せ替えない

## 決定済みの未決事項（2026-10-07）

1. **アニメ済みパラメータへの `SetParameter` で `local_frame` が無いとき**:
   `AnimatedParameter` エラー。現 UI の「定数で潰す」は採らない（エージェントの
   1 回の指示でアニメが黙って消えるため）
2. **同一バッチ内の参照**: 別名 `as` を入れる（決定事項 8）。`AI-2a` から参照型を使う
