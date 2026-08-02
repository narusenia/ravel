# [HIGH-21] NodeEditor が再生中に毎フレーム全再構築される（表示が変わらなくても）

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-app / NodeEditor |
| 該当 | `crates/ravel-app/src/panels/node_editor.rs:612-630`, `:2076-2091`, `crates/ravel-app/src/node_editor/painting.rs:310-320`, `crates/ravel-app/src/project_state.rs:1088-1093` |
| 再調査 | 2026-08-02（原因 2 件は解消済み、1 件は誤り。下記のとおり主因を差し替え） |

## 現状

**2026-08-02 に再調査した。書かれた 3 つの原因のうち 2 つは解消済みで、
残る 1 つは記述が実態と食い違っていた。** 体感の主因は当時の見立てとは別の
ところにある。

### 解消済み: timings グローバルの notify が無条件（旧・原因 1）

`node_editor.rs:612-630` の observer は現在

- `context.is_none()` なら即 return（**閉じているときは notify しない**）
- 表示中のグラフに含まれるノードだけを拾い直し、
  `displayed_timings` と**異なるときだけ** `cx.notify()`

となっている。「閉じても重いまま」の 1 つ目の理由は無くなった。

### 解消済み: `add_node_menu_model` が毎 render（旧・原因 2）

`node_editor.rs:644` で**構築時に 1 回**だけ組み、`render()` は
`self.add_node_menu.clone()`（`Rc` 相当の浅いコピー）を渡すだけになった。
`no_network` 分岐より手前で毎フレーム全テンプレートを sort する経路は無い。

### 記述が誤り: `shape_line` がノード毎・ポート毎（旧・原因 3）

呼び出し箇所（`painting.rs:622`・`:695`・`:1100`）は当時のままだが、
**gpui の `layout_line` は 2 フレーム分のレイアウトキャッシュを持つ**
（`text_system/line_layout.rs:577-602`、キーは text / font_size / runs /
wrap_width / force_width）。ノードラベルとポート名は**フレーム間で文字列が
変わらないのでキャッシュに当たる**。ここは主因ではない。

残るのは `SharedString` の生成と `TextRun` の組み立てが呼び出しごとに走る分で、
ノード数 × ポート数のオーダーではあるが、シェープそのものより 1 桁以上安い。

## 実際に残っている主因

### 主因 A: 再描画のゲートが、それが駆動する表示より細かい

表示は**既に量子化されている**。`painting.rs:310-320` の読み取り値は
10ms 以上なら `{:.0}ms`、未満なら `{:.1}ms`。

ところが notify を決める比較は `HashMap<NodeId, Duration>` の等値で、
**ナノ秒精度の生の `Duration`** を見ている（`node_editor.rs:626`）。

つまり `12.3ms → 12.4ms` は

- 表示は `12ms` のまま**変わらない**
- `Duration` は変わるので `displayed_timings != …` が真になり **notify → 全 render**

再生中はノードごとの実測時間が毎フレーム揺れるので、**表示が 1 文字も
変わらないフレームでも再描画され続ける**。これがタイトルの
「再生中に毎フレーム全再構築」の現在の実体。

### 主因 B: `render()` がノード数ぶんの `HashMap` を毎回組み直す

`node_editor.rs:2076-2091` が毎 render で 2 つ作る。

| 構築物 | 中身 |
|---|---|
| `categories: HashMap<NodeId, NodeCategory>` | ノードごとにレジストリ照会 |
| `labels: HashMap<NodeId, String>` | ノードごとに `node_locale::display_label`（ロケール照会 + `String` 確保） |

どちらも**グラフが変わらない限り不変**なのに、パン・ホバー・ドラッグを含む
あらゆる再描画で作り直される。あわせて `node_sizes` / `displayed_timings` /
`selected_edges` の `clone` も毎回走る。

### 主因 C: `NodeEvalTimings` グローバルは依然 pruning されない

`project_state.rs:1092` は `timings.0.extend(...)` のみ。評価したことのある
全ノードが溜まり続ける。パネル側が表示分だけ拾い直すようになったので
**render のコストには乗らなくなった**が、observer が発火するたびに
グローバル全体を走査して新しい `HashMap` を確保する分は残る。

## 影響

ネットワークを開いた状態での再生中、**表示内容が変わらないフレームでも**
NodeEditor が全再描画される。1 回の render はノード数に比例する
`HashMap` 構築 2 本を含む。閉じていれば notify は止まるので、
「一度重くなったら戻らない」という当初の症状は解消している。

## 修正方針

効果と手間の比が良い順。

1. **notify のゲートを表示の粒度に合わせる**（主因 A）。`displayed_timings` に
   生の `Duration` ではなく**表示に使う丸め済みの値**（あるいは
   `format_duration` の結果そのもの）を持たせ、それが変わったときだけ notify
   する。再生中の大半のフレームで render が消える
2. **`categories` / `labels` をキャッシュする**（主因 B）。どちらもグラフの
   関数なので、`refresh_from_document` が `node_sizes` を作り直すのと同じ
   タイミングで作り直し、`render()` は参照を渡すだけにする
3. **`NodeEvalTimings` を pruning する**（主因 C）。書き込み時に現在の
   ドキュメントに存在しないノードの項目を落とす
4. `node_sizes` / `displayed_timings` を `Rc` で canvas クロージャへ渡し、
   毎 render の `clone` を無くす

## 検証

- 再生中、**表示文字列が変わらない限り** render 回数が増えないことのテスト
  （丸め後の値が同じ 2 つの `Duration` を流して notify されないことを見る）
- ネットワークを閉じた状態で notify が 0 になることのテスト
- `categories` / `labels` の構築回数が render 回数に比例しないことのテスト
- `NodeEvalTimings` の項目数がドキュメントのノード数を超えないことのテスト

## 関連

- [critical/CRIT-01](../critical/CRIT-01-eval-update-notifies-whole-workspace.md) —
  グローバル経由に切り替えた設計。本件はその subscribe 側の絞り込み漏れ
- [medium/ui-rendering.md](../medium/ui-rendering.md) — 他パネルの
  「1 回あたりのコスト」問題
- `docs/implementation/backlog.md`「計画外の課題」の AddNode 検索 UI —
  原因 2 を構造的に解消する
