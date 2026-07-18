# パラメータ InputPort 化実装計画（ノード駆動パラメータ）

対象: task-017-plan.md Phase 7 で繰延した「パラメータの InputPort 化」の復活。
関連要件: REQ-CORE-002（ノードグラフ）、REQ-LAYER-008（rasterize color 入力の
attribute > pin > parameter 優先順位 — 本計画はこれを全ノードに一般化する）。

2026-07-18 の設計セッション（grill-me）でユーザーと合意した決定を反映する。

## 背景

ノードのパラメータを他ノードの出力で駆動する経路は現状 2 つあるが、どちらも
狭く、一般機構が存在しない。

- `ChannelSource::NodeOutput` は evaluator に汎用実装済み
  （`eval.rs` の `resolve_source`）だが **Scalar(f32) 限定**で、UI から
  設定する手段がない。さらにバインディングがグラフのエッジではないため、
  **dirty 伝播（下流 push）が届かず**、上流変更時に下流キャッシュが陳腐化
  し得る構造的問題を抱える。
- rasterize の `color` 入力ピンはノード名直書きの手作り実装
  （`rasterize/mod.rs` の `base_color`）で、他ノードに展開できない。
  結果として `constant.color → rasterize.color` がアプリ内で唯一の合法な
  パラメータ駆動エッジであり、素の `constant`（Scalar 出力）には接続先が
  1 つもない。

## 目的

- 任意のノードの任意のパラメータ（v1 は数値系）を、明示的な公開操作で
  入力ポート化し、エッジで駆動できるようにする。
- dirty 伝播・循環検出・undo・RON 永続化・エッジ描画を**既存の Graph/Edge
  機構にそのまま乗せる**（独自バインディング経路を作らない）。
- 既存 NodeProcessor 実装を**一切変更しない**（evaluator が process 直前に
  パラメータポート入力を ResolvedParams へ織り込む）。

## 目標アーキテクチャ

```text
constant ──edge──▶ [◇ radius]  blur                 Properties パネル
                    └─ is_param InputPort            radius  12.4 (← constant)  ← グレーアウト+評価値
                                                              ◇ 公開トグル
評価時:
eval_node: inputs 収集 → is_param 入力を分離
         → 型変換して ResolvedParams を上書き → process(通常入力のみ)
```

合意済みの設計決定:

1. **実 InputPort + 実エッジ**。公開したパラメータは `node.inputs` 末尾に
   `is_param: true` の実ポートとして追加（`#[serde(default)]`、
   skip_serializing_if 禁止 — bincode ジャーナル制約）。エッジは通常の
   `Edge`。
2. **明示公開**（Houdini の promote 相当）。全自動公開はしない。
3. **型変換 v1 は数値系フル**: Scalar→Float/Channel（そのまま）、
   Scalar→Int（round）、Scalar→Bool（>0.5）、Color→Channel4、
   Vec2→Channel2。String は除外（駆動元ノードがない）。Field は将来の
   per-element 駆動用に予約し v1 では接続不可。
4. **接続中のパラメータは編集不可**。Properties は現フレームの評価値を
   グレー表示し「← 接続元ノード名」を注記。ノード内の stored 値は
   fallback として保持し、エッジ削除で復帰・編集可に戻る。
5. **公開操作はノード右クリック「Expose Parameter ▸」（トグル、公開済みは
   チェック表示）と Properties パラメータ行の小ポートアイコンの両方**。
6. **非公開化は接続エッジごと削除**し 1 undo ステップにまとめる。

## Phase 1: コアモデルと Graph 操作

### 主な対象

- `crates/ravel-core/src/graph.rs`（InputPort、Node、Graph）
- `crates/ravel-core/src/recovery/`（ジャーナル版数）

### 作業

- `InputPort` に `is_param: bool` を追加（`#[serde(default)]`）。
  bincode ジャーナルはフィールドレイアウト変化で版跨ぎ互換がないため、
  ジャーナルのフォーマット版数を上げ、旧版は「復旧不能・破棄」として扱う
  （既存の版数検証経路に乗せる）。
- パラメータ型 → 受理型の導出関数:
  Float/Channel/Int/Bool → `SCALAR`、Channel4 → `COLOR`、
  Channel2 → `VEC2`。Channel3 と String は v1 では公開不可。
- `Graph::expose_param_port(node_id, key) -> Result<Graph>`:
  対象パラメータの存在と型を検証し、`is_param` ポートを inputs 末尾に
  追加した新 Graph を返す。既公開・不明キー・非対応型はエラー。
- `Graph::remove_param_port(node_id, key) -> Result<Graph>`:
  ポート除去 + そのポートへの接続エッジ削除 + **後続ポートへ接続する
  全エッジの `target_port` 再インデックス**を 1 つの Graph 操作として
  原子的に行う（undo 単位は呼び出し側の Document commit 1 回）。
- 公開対象は user ノードのみ。synthetic（comp.\*）、net.in/net.out、
  subnet は expose を拒否（subnet は既存の「未接続ピン→パラメータ昇格」と
  意味論が衝突するため v1 対象外）。
- validate: `is_param` ポートは対応するパラメータキーと同名であることを
  検証項目に追加。循環検出は既存の add_edge 検証にそのまま乗る。

### 完了条件

- expose/remove の Graph 操作がヘッドレステストで検証されている
  （再インデックス、エッジ同時削除、非対応型の拒否、synthetic 拒否を含む）。
- ジャーナル版数が上がり、旧版ジャーナルが安全に破棄される。
- `cargo test -p ravel-core` が通る。

## Phase 2: Evaluator のパラメータ解決

### 主な対象

- `crates/ravel-core/src/eval.rs`

### 作業

- `eval_node` の入力収集後、`is_param` ポートの入力値を分離し、
  data 入力のみを `process` に渡す（inputs の位置インデックスは
  param ポートが末尾 append のため data ポートに対して不変。merge のような
  全入力走査プロセッサにも param 入力が混入しない）。
- 分離した値を型変換して `ResolvedParams` を上書き:
  Scalar→f32（Float/Channel 系）、round→Int、>0.5→Bool、
  Color→Channel4 相当、Vec2→Channel2 相当。ダウンキャスト失敗は
  パラメータ fallback にフォールバックし warn ログ。
- 優先順位を明文化: **attribute > param ポート > stored パラメータ**
  （rasterize color の既存規約を一般化）。rasterize の手作り `color` ピン
  実装はこの一般機構の上に統合し、既存の golden/等価テストを維持する
  （`color` ピンは「最初から公開済みの param ポート」として扱う）。
- `ChannelSource::NodeOutput` は現状維持（deprecated コメントを付け、
  UI からは生成しない。アニメーション合成レベルのバインディングとして
  将来再評価）。

### 完了条件

- constant→blur.radius、constant→shape.polygon.sides（round）、
  constant.color→rasterize.color、constant→transform.rotation の駆動が
  ヘッドレステストで検証されている。
- 上流 constant の値変更（Params ヒント）で下流が再評価される
  （dirty 伝播がエッジ経由で機能）ことをテストで確認。
- shape_layer_golden・GPU/CPU 等価テストが無変更で通る。

## Phase 3: ノードエディタ UI

### 主な対象

- `crates/ravel-app/src/panels/node_editor.rs`
- `crates/ravel-app/src/node_editor/painting.rs`
- `assets/locales/en.toml` / `ja.toml`

### 作業

- ノード右クリックメニューに「Expose Parameter ▸」サブメニュー
  （対象ノードの公開可能パラメータを列挙、公開済みはチェック表示、
  選択でトグル）。公開/非公開それぞれ 1 Document undo ステップ。
- param ポートの描画: 通常の入力ポートとして描画し（型色は既存の
  port_colors）、ラベルにパラメータ名を表示。形状で区別する場合は
  ダイヤ形等の差別化を検討（v1 は同形状+ラベルで可）。
- 接続スナップ・型フィルタ・単一入力制約・エッジ作成/削除は既存機構が
  そのまま適用される（accepted_types 照合）。
- メニュー文言は `t!` + en/ja ロケール追加。

### 完了条件

- 公開 → 接続 → 値駆動 → 非公開（エッジごと消滅）が UI 操作で一巡し、
  各段が undo/redo で往復できる。
- ヘッドレスで表現可能なメニューモデル（公開可能列挙・チェック状態）に
  テストがある。

## Phase 4: Properties パネル連携

### 主な対象

- `crates/ravel-app/src/panels/properties.rs`
- `crates/ravel-ui/src/properties/node.rs`

### 作業

- 接続済みパラメータの行: ウィジェットを無効化し、現フレームの評価値を
  グレー表示 + 「← 接続元ノード名」を注記。
  評価値の取得は EvalService の完了スナップショットに「解決済み
  パラメータ値」を同梱する方式を第一候補とし、実装コストが見合わない
  場合は v1 として「上流が constant/constant.color のときのみ実値、
  それ以外は "connected" 表示」に縮退してよい（縮退した場合は本計画に
  追記して残課題化する）。
- パラメータ行の左に公開トグルアイコン（未公開=淡色ドット、公開済み=
  型色ドット、接続済み=塗りつぶし）。クリックで公開/非公開
  （非公開はエッジごと削除の確認なし・undo 可）。
- `node_params_section` に公開/接続状態を渡し、`PropertyField` に
  読み取り専用化と注記を表現するメタデータを追加する
  （例: `PropertyField` 各 variant に `driven: Option<String>` を足すか、
  section 側の別マップで渡すかは実装時に判断）。

### 完了条件

- 接続済みパラメータが Properties で編集不可+注記付き表示になり、
  エッジ削除で編集可能に復帰する（gpui テスト）。
- 行アイコンからの公開/非公開がノードエディタ経由と同じ undo 粒度で
  動作する。

## 実装単位と順序

1. InputPort.is_param + Graph expose/remove 操作 + ジャーナル版数（Phase 1）
2. evaluator 分離解決と型変換、rasterize color の統合（Phase 2）
3. ノードエディタの公開メニューとポート描画（Phase 3）
4. Properties の接続表示と行トグル（Phase 4）

Phase 1+2 が第一マイルストーン（コア層で完結、UI なしで検証可能）。
Phase 3+4 が第二マイルストーン。

## 影響ドキュメント

- `docs/specifications/data-model.md`（InputPort.is_param、公開ポートの
  意味論、優先順位規約）
- `docs/specifications/architecture.md`（evaluator のパラメータ解決順）
- `docs/agent-api-reference.md`（Graph::expose_param_port /
  remove_param_port、公開 API 変更）

## リスクと注意

- **bincode ジャーナル互換**: InputPort へのフィールド追加はレイアウト
  変化。版数 bump と旧版破棄の経路を Phase 1 で必ず先に入れる。
  `#[serde(skip_serializing_if)]` は禁止（既知の罠）。
- **ポート index 再マップ**: remove 時の target_port シフトは Graph 操作
  内で原子的に処理。エッジ再マップ漏れは即データ破壊なので Phase 1 の
  テストで網羅する。
- **merge 等の全入力走査プロセッサ**: param 入力の分離を evaluator で
  行うため process には届かない設計だが、等価テストで回帰を確認する。
- **rasterize color の統合**: 既存 golden をピン留めしたまま一般機構に
  載せ替える。挙動差が出る場合は統合を Phase 2 から切り出して単独 PR に
  する。
