# [HIGH-21] NodeEditor が再生中に毎フレーム全再構築される（ネットワークを閉じていても）

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-app / NodeEditor |
| 該当 | `crates/ravel-app/src/panels/node_editor.rs:534`, `:1644-1690`, `:120-148`, `crates/ravel-app/src/node_editor/painting.rs:556,628,1014`, `crates/ravel-app/src/project_state.rs:941-949` |

## 現状

原因が 3 つ独立して重なっている。**ノード 10 個程度でも体感できる**。

### 原因 1: timings グローバルの notify が無条件

`ProjectState::on_eval_update` は評価結果が届くたびに `NodeEvalTimings`
グローバルを書き換える（`project_state.rs:941-949`）。再生中は毎フレーム。

NodeEditor はこれを observe して**無条件に `cx.notify()`** する。

```rust
// node_editor.rs:534
let timings_sub =
    cx.observe_global::<crate::project_state::NodeEvalTimings>(|_this, cx| cx.notify());
```

表示中のグラフに含まれないノードのタイミングが来ても notify する。
**ネットワークを閉じている（`context: None`）ときも notify する。**

これは CRIT-01 の修正方針（「評価出力が必要なパネルはそれを運ぶグローバルを
subscribe する」`project_state.rs:935-940`）に沿った実装だが、
subscribe した先で絞り込みをしていない。

### 原因 2: `render()` が毎回フル再構築、しかも閉じた状態でも走る

`render()`（`:1644-1690`）は毎回:

- `self.graph.clone()` / `self.node_sizes.clone()` / `timings.0.clone()`
- `categories: HashMap<NodeId, NodeCategory>` をレジストリ照会で構築
- **`add_node_menu_model(&self.registry)`（`:1670`）**

`add_node_menu_model`（`:120-148`）はレジストリの全テンプレート（現在 48 個）を
カテゴリごとに集め、`label` と `type_key` を `String` clone し、ラベルで sort する。
これが**毎 render**。

さらにこの行は `no_network` 分岐（`:1690-1707`）より**手前**にあるため、
**ネットワークを閉じていても毎フレーム走る**。

### 原因 3: `shape_line` がノード毎・ポート毎

`painting.rs` はテキストを 3 箇所でシェープする。

| 箇所 | 対象 |
| --- | --- |
| `:556` | ノードラベル |
| `:628` | **ポート名（ポートのループ内）** |
| `:1014` | 処理時間の読み取り値 |

ノード 10 個 × ポート 4 個で 50 回超/フレーム。うち `:1014` の処理時間は
**テキストが毎フレーム変わる**（`12ms` → `13ms`）ため、同じ内容を前提とする
シェープキャッシュに乗らない。

## 「閉じても重いまま」の理由

`close_network`（`:705-716`）は `self.graph` と `node_sizes` をクリアするが、

1. `timings_sub` の notify は止まらない（原因 1）
2. `add_node_menu_model` の再構築は閉じていても走る（原因 2）
3. **`NodeEvalTimings` は一度も pruning されない**。
   `project_state.rs:947` は `timings.0.extend(...)` するだけなので、
   評価したことのある全ノードが溜まり続ける。その HashMap を毎 render
   clone する（`node_editor.rs:1674`）

3 が「直前に大量のノードを開いていたときだけ重さが残る」という
ノード数依存の残留を説明する。

## 影響

再生中および任意のパラメータ編集中、NodeEditor が常に最大コストで再描画される。
ネットワークを閉じても解消しないため、ユーザーには「一度重くなったら戻らない」
と見える。第1段（RESP-1〜3）で評価回数を減らした効果を、このパネルだけが
打ち消している。

## 修正方針

原因ごとに独立して直せる。効果と手間の比が良い順:

1. **timings notify を絞る**。`context.is_none()` なら notify しない。
   表示中のグラフに含まれるノードの値が実際に変わったときだけ notify する
2. **`add_node_menu_model` を `render()` から外す**。レジストリは不変なので
   構築時に 1 回で足りる。メニューを検索 UI に置き換える案
   （`docs/implementation/backlog.md`「計画外の課題」）を採るなら、そちらで
   構造的に消える
3. **処理時間表示を量子化する**。表示値を丸めて前フレームと同じなら
   文字列を作り直さない。または更新頻度を 4 回/秒程度に落とす
4. **`NodeEvalTimings` を pruning する**。書き込み時に現在のドキュメントに
   存在しないノードの項目を落とす
5. `timings` / `node_sizes` の clone を避け、`Rc` で canvas クロージャへ渡す

## 検証

- 再生中に NodeEditor の render 回数を数える計装を入れ、ネットワークを
  閉じた状態で 0 になることのテスト
- `add_node_menu_model` の呼び出し回数が render 回数に比例しないことのテスト
- 処理時間表示が量子化されていることのテスト（同じ丸め値なら文字列を
  再生成しない）
- `NodeEvalTimings` の項目数がドキュメントのノード数を超えないことのテスト

## 関連

- [critical/CRIT-01](../critical/CRIT-01-eval-update-notifies-whole-workspace.md) —
  グローバル経由に切り替えた設計。本件はその subscribe 側の絞り込み漏れ
- [medium/ui-rendering.md](../medium/ui-rendering.md) — 他パネルの
  「1 回あたりのコスト」問題
- `docs/implementation/backlog.md`「計画外の課題」の AddNode 検索 UI —
  原因 2 を構造的に解消する
