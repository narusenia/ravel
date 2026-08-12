# パネル可視性の配線 実装計画

> **Status**: Planned — `MED-UI-02` の残り、フェーズ C3 の `RESP3-7` 未達分
>
> この文書は設計ゲート用の実装計画である。この計画そのものでは `crates/`
> 配下のコードを書かない。実装時は単位ごとに分割し、各単位の完了条件を
> 満たしてから次へ進む。

## 問題

**タブの裏に隠れているパネルが、見えないまま毎回作り直されている。**

`ravel-dock` は `LayoutNode::Area { tabs, active }` でどのタブが手前かを知って
いて、描画するのは手前の 1 枚だけである（`crates/ravel-dock/src/dock.rs:610` の
`self.content.view(&tabs[active], …)`）。だが**パネルのエンティティは死なない** —
`PanelViews.views` がインスタンス id ごとにキャッシュし
（`crates/ravel-app/src/panels/mod.rs:1150` 付近）、`retain` が落とすのは
「どのウィンドウにも存在しなくなったインスタンス」だけである
（同 `:1179` 付近）。したがって**裏のタブのパネルも observer が全部走る**。

裏のパネルが払っているもの:

| パネル | 裏でも走る仕事 |
|---|---|
| Properties | `refresh_values` — ターゲット再解決 + 全セクション再構築 |
| Timeline | `sync_from_project` — 鏡の更新 |
| Outliner / MediaBin | `rebuild_rows` — 行モデルの再構築 |
| NodeEditor | `refresh_from_document` |

フェーズ C3 はこれらの**1 回あたりのコスト**を落としたが、**回数そのものは
可視性と無関係のまま**である。`RESP3-7` の完了条件のうち
「パネル非表示のとき `refresh_values` を走らせない」だけが未達で、
それがフェーズ C3 を `完了` にできない唯一の理由になっている
（`roadmap.md` のフェーズ C3 節）。

### なぜ今まで書けなかったか

**可視性がパネルへ届く経路が無い。** `active: usize` はレイアウトツリーの中に
あり、パネル側からは見えない。`PaneContent::view()` は手前のタブについてだけ
呼ばれるので「呼ばれた = 見えている」と読めそうだが、**それは render 経路**で
あり、そこでパネルの状態を書き換えるのは `.agents/rules/gpui.md` の render
純粋性に反する。

## 目標アーキテクチャ

### 可視集合を Global に置き、レイアウトが変わったときだけ書く

```text
WindowHost::show_tree(root)        ← レイアウトツリーが変わる唯一の入口
        │
        ├─ このウィンドウの「各エリアの手前のタブ」を集める
        ▼
   VisiblePanels(HashSet<PanelInstanceId>)     ← Global
        │
        ▼
   各パネルの observe_global                   ← 可視化の瞬間だけ発火
```

`.agents/rules/gpui.md` の Global 分類では「耐久的な共有状態」に当たる —
one-shot イベントではなく、ウィンドウが開いている限り意味を持つ状態である。
`FocusedPanelGlobal` と同じ性格で、書き手は 1 箇所（`show_tree`）に閉じる。

**レイアウトツリーを直接 observe させない。** ツリーはウィンドウごとに
`WindowHost` が持つので、パネルが横断して読むと「どの `WindowHost` か」を
パネルが知る必要が出る。可視集合はウィンドウを跨いだ 1 枚の事実なので、
Global に畳むほうが依存が少ない。

### ゲートは「飛ばす」ではなく「遅らせる」

**裏で飛ばした更新は、表に戻った瞬間に取り返さなければならない。**
飛ばしたまま放置すると、タブを切り替えた瞬間に古い値が見える —
性能のために正しさを捨てる取引になる。

既にある `panels::MirrorEpoch`（`crates/ravel-app/src/panels/mod.rs:960`）が
この形をしている: 「自分が最後に同期した世代」を持ち、進んでいたら同期する。
裏で飛ばすと epoch が進まないので、**表に戻ったときに 1 回同期すれば
それだけで追いつく。**

```text
notify 到来
  ├─ 可視     → 従来どおり同期（epoch を記録）
  └─ 不可視   → 何もしない（epoch も記録しない = 借りが残る）

可視集合が変化
  └─ 自分が 不可視 → 可視 に変わった → 同期を 1 回強制
```

`MirrorEpoch` が「借り」を勝手に表現してくれるので、**別途 dirty フラグを
足さない。** epoch を記録しないこと自体が借りである。

### 対象にしないパネル

- **Viewer** — 裏にいても評価要求を出し続ける必要がある（再生とキャッシュの
  先読み）。「見えないから止める」は Viewer では機能の停止になる
- **スコープ類**（Waveform / Vectorscope / Histogram）— 現時点で
  ドキュメントを鏡にしていない。将来鏡を持ったら同じゲートに乗せる

## 実装単位

| 単位 | 内容 | 依存 |
|---|---|---|
| `VIS-1` | `VisiblePanels` Global と `WindowHost` からの維持（挙動不変） | — |
| `VIS-2` | 可視性ゲートの共有ヘルパと Properties への適用（`MED-UI-02`） | `VIS-1` |
| `VIS-3` | Timeline / Outliner / MediaBin / NodeEditor への適用 | `VIS-2` |
| `VIS-4` | 仕様・実装状況・測定手順の文書 | `VIS-3` |

## 単位ごとの完了条件

### `VIS-1` `VisiblePanels` Global と維持

- `panels::VisiblePanels`（`HashSet<PanelInstanceId>`）が
  `crates/ravel-app/src/panels/mod.rs` に入り、`Global` を実装する
- `WindowHost::show_tree`（`crates/ravel-app/src/window_host.rs:983` 付近）が、
  そのウィンドウの**各エリアの手前のタブ**を集めて Global の自分のウィンドウ分を
  置き換える。**書き手はここ 1 箇所**
- ウィンドウが閉じるとき、そのウィンドウの分が集合から消える
  （`WindowRegistry` の登録解除と同じ経路で）
- **この単位では誰も読まない。** 挙動は完全に不変であること
- 次を落とすテストがある:
  - タブを切り替えると、前の手前が外れ新しい手前が入る
  - 分割すると両エリアの手前が入る（**1 ウィンドウに複数の可視パネル**）
  - タブを閉じると外れる
  - 検出したウィンドウのパネルも可視集合に入る（デタッチしても見えている）
  - ウィンドウを閉じるとその分だけが消え、他ウィンドウの分は残る

### `VIS-2` 可視性ゲートと Properties

- パネル側の共有ヘルパ（`MirrorEpoch` の隣に置く）が次を提供する:
  - 「自分は今見えているか」
  - 「不可視 → 可視 に変わったか」を通知する購読
- Properties が `refresh_values` を可視のときだけ走らせる。
  **`PlaybackPosition` observer と project observer の両方**に効くこと
- **不可視 → 可視 の遷移で 1 回同期する。** タブを戻した直後の 1 フレーム目で
  値が古い経路が無いこと
- 次を落とすテストがある:
  - 裏にいる間はドキュメント編集で `refresh_values` が走らない
  - **表に戻すと、裏で起きた編集が反映される**（これが本体。飛ばした更新を
    取り返せていなければ落ちる）
  - 裏にいる間の再生（`PlaybackPosition` 30 回）で `refresh_values` が 0 回
  - 表に戻したあと、通常どおりプレイヘッドに追従する
- `RESP3-5` の計装（`crates/ravel-app/src/panels/sync_probe.rs`）で
  before / after を測り `perf-baseline.md` に記録する
- **テストに歯があることを確認する**: 「不可視 → 可視 の強制同期」を外すと
  上の 2 番目が落ちること。確認結果を PR 本文に書く

### `VIS-3` 残り 4 パネル

- Timeline `sync_from_project`、Outliner / MediaBin `rebuild_rows`、
  NodeEditor `refresh_from_document` が同じゲートに乗る
- **Viewer は対象外**（上の理由）。対象外であることをコードのコメントに書く
- 各パネルについて `VIS-2` と同じ 2 本（裏で走らない / 表に戻すと追いつく）を
  落とすテストがある
- **グローバル駆動の sync**（`ActiveComposition` / `SelectedPropertiesTarget`）が
  不可視パネルでどう振る舞うかを決めて書く。`RESP3-11` で入れた epoch 記録と
  噛み合うこと — **裏で epoch だけ記録して同期を飛ばすと借りが消えて
  古いまま残る**ので、そこが最大の落とし穴
- 5 パネルすべてを裏に置いた状態でドラッグ 10 move を流し、sync 回数の合計が
  0 になることを計装で示す

### `VIS-4` 文書

- `docs/specifications/ui/` の該当パネル仕様に、裏のタブでは更新を遅らせ
  表に戻ったときに追いつく、と書く
- `docs/ui-impl-status.md` を更新する
- `perf-baseline.md` に測定手順（どのテストが何を測るか）を追加する
- `issues/medium/ui-rendering.md` の `MED-UI-02` を閉じ、
  `issues/closed/medium-ui-rendering.md` へ移す
- `roadmap.md` のフェーズ C3 を `完了` にする。**これが C3 を閉じる最後の単位**

## やらないこと / 見送る選択肢

- **ウィンドウの最小化・遮蔽を可視性に含めない。** GPUI のウィンドウ活性は
  別の信号で、背面ウィンドウを不可視扱いにすると「ウィンドウを前面に出した
  瞬間に古い」経路が増える。裏のタブだけを扱う。必要になったら別単位で足す
- **`PaneContent` にコールバックを足さない。** ホストからパネルへ
  `set_visible` を押す形は、呼ぶ場所が render 経路の近くになり render
  純粋性を崩しやすい。読む側が Global を観測する形にする
- **パネルのエンティティを破棄しない。** 裏のタブでエンティティを落とすと
  タブを戻したときにビュー状態（スクロール位置、展開状態、進行中のジェスチャー）が
  消える。`PanelViews` が `retain` でしか落とさないのはそのため
- **dirty フラグを別に持たない。** `MirrorEpoch` を記録しないこと自体が
  借りの表現で、二重に状態を持つと片方だけ更新する事故が入る
- **Viewer を止めない**（上記）

## ロードマップ上の位置づけ

フェーズ C3「応答性の残り」の最後の 1 枚。`RESP3-7` の完了条件のうち
「パネル非表示のとき `refresh_values` を走らせない」だけが未達で、
`VIS-4` がそれを満たした時点でフェーズ C3 が `完了` になる。

C3 の他の単位とは独立しているので、いつ着手してもよい。

## 関連文書

- `responsiveness-stage3-plan.md`（`RESP3-7` の完了条件）
- `roadmap.md` フェーズ C3
- `done/free-pane-docking-plan.md`（`DOCK-*` — レイアウトツリーとタブの由来）
- `issues/medium/ui-rendering.md` `MED-UI-02`
- `perf-baseline.md`（sync 回数の測り方）
