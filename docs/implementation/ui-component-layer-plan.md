# UI コンポーネント層と UX 不変条件の計画

> **Status**: 未着手 — 2026-09-07。`UIX-0` / `UIX-1` は `KIT-1` と衝突しないので
> 先行できる。`UIX-2` 以降は **`KIT-1` 待ち**（`gpui-base` がツリーに入るまで
> 部品を書けない）。

## 背景

Ravel の UI は「動く」ところまで来ている（`docs/ui-impl-status.md` で ✅ が
198 項目）。だが**質感と一貫性を誰も所有していない**。実測した現状:

- **Ravel 独自のコンポーネントは 6 ファイル / 6189 行**
  （`ravel-app/src/widgets/`: `curve_editor` / `curve_view` /
  `param_curve_editor` / `param_ramp_editor` / `scrub_input`）。
  モジュール doc は「gpui-component independent」と自称しているが、
  **`ravel-app` の中に居るので単体で動かせず、gallery も無い**
- **トークン層が無い。** gpui-component のテーマから読んでいるのは**色 10 種
  だけ**（`muted_foreground` 12 / `border` 7 / `foreground` 4 / `danger` 3 /
  `primary` 2 / `accent` 2 / `secondary` / `info` / `drop_target` /
  `background`）。**間隔・字送り・モーションのトークンは 1 つも無い**
- **ハードコードされた色が 24 箇所**（`properties.rs` 6 / `param_ramp_editor` 4
  / `viewer` 系 10 / `composition_form` 3 / `panels/mod.rs` 1）
- **行高が場所ごとに違う。** `outliner` 22 / `attribute_spreadsheet` 22 /
  `palette` 26 / `media_bin` 28 / `timeline` のレイヤー行 28・プロパティ行 20。
  ヘッダは 24 で揃っているが、これは偶然揃っているだけで根拠が無い
- **アクセシビリティは実質ゼロ。** `tab_index` 0 / `tab_stop` 0 /
  `accessibility_id` 0。`focus_handle` は 115 箇所あるがパネル単位の
  コマンド経路用で、**Tab 順という概念が存在しない**
- **ライトテーマがディスク上にあって到達できない**
  （`assets/themes/ravel.json` に light / dark 両方、`AppearanceMode` に
  System もあるが、`MED-APP-10` が「設定レイヤーは永続化されるが一切
  適用されない」）

`issues/` の open な UI 所見も、見た目の問題ではなく**同じ不変条件を
別々の場所で破っている**という形をしている（下の表）。だから
「パネルを 1 枚ずつ綺麗にする」のではなく、**不変条件を決めて、
それを機械的に守れる土台を作る**のが安い。

## 目的

1. **Ravel の部品が住む場所を作る** — `ravel-widgets` クレート。
   `ravel-dock` と同じ形（GPUI 依存の独立クレート + `examples/gallery`）
2. **トークンを 1 箇所に集め、リテラルを lint で禁止する**
3. **UX の不変条件を文書化し、違反している既存 issue を潰す順序を決める**
4. **キーボードだけで到達・起動・離脱できる**ことを部品の条件にする

## この計画が引き受けないもの

- **パネルごとの再デザイン。** 情報階層や配置の見直しは対象外。
  この計画は「部品と不変条件」までで、`Node Editor` の絵をどうするかは
  別の単位
- **個別バグの詳述。** `issues/` にある記述が正。この計画は
  「どの不変条件に対応するか」と「潰す順序」だけを持つ
- **裾の 8 部品**（`TitleBar` / `TabBar` / `Slider` / `Radio` / `Progress` /
  `Popover` / `DataTable` / `Accordion`。各 1 箇所）。gpui-kit のまま使う
- **`Input` の再実装。** IME・テキスト選択・undo を自前で持つ費用が
  見合わない。上流 gpui の `view_example/example_input.rs` に実装例が
  入ったので不可能ではないが、この計画では借りる
- **スクリーンリーダーでの読み上げ検証。** `accessibility_id` を付ける
  ところまで。VoiceOver の通し操作は完了条件にしない

## 決めたこと（設計言語）

### 基準は「DCC の密度、現代の質感」

情報密度と操作体系は DCC のまま（数値スクラブ、行高 20px 前後、暗色基調）。
その上に**トークン化された階調**と**意図のあるモーション**と
**見えるフォーカス**を乗せる。上流 gpui に入った `Animated<T>` /
`use_keyed_transition` / `style_transitions` がそのまま使える。

Properties の 1 行が到達すべき状態:

```
  Position          │  1920.0  │  1080.0  │
  ╰─ ホバーで ↕ カーソル、ドラッグでスクラブ
     ドラッグ中は背景が持ち上がり、離すと戻る（値の変化は即時）
     フォーカスは 1px のアクセントリング
     連動トグル、成分ラベル X / Y を持つ
```

### モーションは状態のフィードバックだけ

**動かしてよいのは `hover` / `focus` / `press` / `disabled` の見た目だけ。**
100〜180ms。**値・レイアウト・パネルの出入りは即時。**

理由: スクラブと再生が 60fps で走るツールでは、レイアウトのアニメーションは
フレーム予算と競合し、そのまま遅延に見える。DCC で「入力した値がその場で
反映されない」のは欠陥として扱われる。

例外は**名指しで列挙する**（この一覧に無いものは動かさない）:

| 動かしてよいもの | 理由 |
|---|---|
| トーストの登場と退場 | 何も無かった場所に現れるので、出現位置を目で追う手がかりが要る |
| ドロップターゲットの点灯 | ドラッグ中のフィードバックそのもの |
| ホバー / フォーカス / 押下 / 無効 の見た目 | 上記 |

### 密度の基準値

行高は**用途で 2 段**に統一する。今バラバラな 5 種を潰す:

| トークン | 値 | 用途 | 今の値 |
|---|---|---|---|
| `row.compact` | 20px | 値の行（Properties のパラメータ、Timeline のプロパティ行） | 20（timeline）だけ正しい |
| `row.default` | 24px | 一覧の行（Outliner / MediaBin / Palette / Spreadsheet） | 22 / 28 / 26 / 22 |
| `header` | 24px | パネルヘッダ | 24（既に揃っている） |

`row.default` を 24 に寄せるのは、**ヘッダ 24 と合わせて 1 つの階段**に
するため（22 / 24 / 26 / 28 の 4 段は意味の差ではなく実装の差だった）。

## 目標アーキテクチャ

```
crates/ravel-widgets/          ← 新規
  src/
    tokens.rs        色・間隔・字送り・モーションの唯一の出どころ
    button.rs        gpui-base の primitive から自前
    icon.rs          同
    tooltip.rs       同
    curve_editor.rs  ravel-app から移設（1391 行）
    curve_view.rs    同（291 行）
    param_curve_editor.rs  同（2778 行）
    param_ramp_editor.rs   同（1146 行）
    scrub_input.rs   同（558 行）
  examples/
    gallery.rs       全部品を全状態で並べる（実機の目視はここで行う）
```

- **依存は `gpui` + `gpui-base` + `ravel-i18n`。`gpui-component` に依存しない**
  （`Input` を借りるのは `ravel-app` 側で、部品層は借りない）
- `ravel-app` は `ravel-widgets` を使う。パネルは**トークン以外の色・間隔を
  書かない**
- **`ravel-ui` にトークンは置けない** — あちらは GUI-free（`gpui` に依存
  しない）ので `Hsla` を持てない。トークンは `ravel-widgets` の中

### 部品ごとの方針

| 部品 | 箇所 | 方針 |
|---|---|---|
| `Icon` | 58 | **自前**（`gpui-base` の primitive から。SVG の解決とサイズ量子化は既に `node_editor/painting.rs` が持っている知見を使う） |
| `Button` | 51 | **自前**（質感が最も宿る。ホバー・フォーカス・押下・無効の 4 状態と、Tab 順・Enter / Space 起動を自分で持つ） |
| `Tooltip` | 21 | **自前**（登場の遅延と位置決めが UX そのもの） |
| `Input` | 20 | **借りる**（IME・テキスト選択・undo。gpui-kit のまま） |
| 裾 8 部品 | 各 1 | **触らない** |

## UX の不変条件

**この 12 個がこの計画の成果物である。** `docs/dev/` に置き、
`ravel-review` の検査手順に入れる。右列は「今それを破っているもの」。

| # | 不変条件 | 違反している issue |
|---|---|---|
| 1 | **選択の所有権** — パネルは自分が所有しない選択を書き換えない | `MED-APP-05` / `MED-APP-06` |
| 2 | **選択の寿命** — 文脈が変わったら捨てる。stale な選択を残さない | `MED-APP-04` |
| 3 | **1 操作 1 undo。** 変化が無ければステップを作らない | `MED-APP-07` / `LOW-APP-02` |
| 4 | **ドラッグはいつでも取り消せる** — Escape / ボタン喪失 / 対象消失のどれでも | `MED-APP-03` |
| 5 | **どの幅でも操作できる** — 最小幅を宣言し、切れる側を決める | `MED-UI-07` |
| 6 | **見えている操作は必ず何かする** — できないなら無効化して見せる | `MED-APP-17` |
| 7 | **値の意味が編集器に出る** — 成分名・単位・型 | `MED-APP-19` / `MED-APP-20` / `MED-APP-29` / `MED-APP-30` |
| 8 | **設定は書けたら効く** | `MED-APP-10` |
| 9 | **文書が変わったら派生キャッシュを捨てる** | `MED-APP-08` |
| 10 | **キーボードだけで到達・起動・離脱できる** — Tab 順 / Enter・Space / Escape / 見えるリング | 該当 issue なし（`tab_index` が 0 なので起票されていない） |
| 11 | **状態のフィードバックだけ動く。** 値・レイアウトは即時 | 該当なし（モーションが無いので破れていない） |
| 12 | **色・間隔・字送りはトークン経由。** リテラルを書かない | ハードコード 24 箇所 |

11 と 12 は**新しく守るもの**、1〜10 は**既に破られているもの**である。

## 単位

| ID | 単位 | 依存 |
|---|---|---|
| `UIX-0` | 不変条件を `docs/dev/ux-invariants.md` に文書化し、`ravel-review` の検査手順に入れる | — |
| `UIX-1` | トークンの型定義と値の決定（色・間隔・字送り・モーション）。**まだ配線しない** | — |
| `UIX-2` | `ravel-widgets` クレートを切り、既存 6189 行を移設。`examples/gallery` を作る | `KIT-1` |
| `UIX-3` | `tokens.rs` を配線し、ハードコード 24 箇所を潰す。`lint-patterns.sh` にリテラル禁止を追加 | `UIX-1` / `UIX-2` |
| `UIX-4` | `Icon` / `Button` / `Tooltip` を `gpui-base` から自前で作る。4 状態 + Tab 順 + Enter / Space | `UIX-2` |
| `UIX-5` | 行高を 2 段に統一（`row.compact` 20 / `row.default` 24） | `UIX-3` |
| `UIX-6` | **不変条件 1〜4 の違反を潰す**（選択の所有権・寿命、undo の粒度、ドラッグの取り消し） | `UIX-0` |
| `UIX-7` | **不変条件 5〜9 の違反を潰す**（狭い幅、死んだ操作、値の意味、設定の適用、派生キャッシュ） | `UIX-0` |
| `UIX-8` | ライトテーマを到達可能にする（`MED-APP-10` の設定適用経路） | `UIX-3` |
| `UIX-9` | 文書更新（`ui-impl-status.md` の密度・部品の記述、`gpui-ui-guide.md` の「部品を追加する」節） | `UIX-4`〜`UIX-8` |

**`UIX-0` と `UIX-1` はパネルを触らないので `KIT-1` と並行できる。**
`UIX-2` 以降は `gpui-base` がツリーに入るまで書けない。

---

## Phase 0: 不変条件とトークンの確定（`UIX-0` / `UIX-1`）

`KIT-1` と衝突しない。文書と型定義だけ。

### 作業

- `docs/dev/ux-invariants.md` に 12 個を書く。**各項目に「破ったときに
  どう見えるか」を 1 行**添える（抽象的な原則だけだと検査に使えない）
- `ravel-review` スキルの検査手順に「不変条件の照合」を足す
- トークンの型を決める。**色は既存の 10 種 + `category_color` の族**から
  始め、増やすのは実際に要ったときだけ（先回りして 40 色作らない）
- 間隔は 4px 刻み、行高は上の 2 段、字送りは既存の
  `theme().font_family` / `mono_font_family` に段を足す形
- モーションは `Motion` 2 種だけ（`feedback.in` 120ms / `feedback.out` 180ms）

### 完了条件

- 不変条件 12 個が `docs/dev/` にあり、各項目に違反時の見え方が書かれている
- `ravel-review` の手順から辿れる
- トークンの型が `mise run check` を通る（値は入っているが誰も読まない状態）
- **`mise run docs:check` が通る**

---

## Phase 1: 部品層の器（`UIX-2`）

**`KIT-1` 待ち。**

### 作業

- `crates/ravel-widgets` を作る。依存は `gpui` / `gpui-base` / `ravel-i18n`
- `ravel-app/src/widgets/` の 5 ファイル（6189 行）を移す。
  **移設だけで挙動を変えない**（`properties.rs` と `timeline.rs` の
  import が変わるだけのコミットに切る）
- `examples/gallery.rs` を作る。`ravel-dock/examples/gallery.rs` と同じ形

### 完了条件

- `cargo build -p ravel-widgets` が通り、**`gpui-component` に依存しない**
  （`cargo tree -p ravel-widgets` で確認）
- 移設前後で `mise run check` の結果が同じ
- `cargo run -p ravel-widgets --example gallery` が起動し、
  移設した 5 部品が全部出る
- **`ravel-app` の挙動が 1 つも変わらない**（既存テストが無改変で通る）

---

## Phase 2: トークンの配線と lint（`UIX-3` / `UIX-5`）

### 作業

- `tokens.rs` を配線し、**ハードコード 24 箇所を潰す**
- `scripts/lint-patterns.sh` に追加:
  - `tokens.rs` 以外での `rgb(0x` / `rgba(0x` / `hsla(`
  - `tokens.rs` 以外での生の `px(<数値>)`（レイアウト定数）
- **例外は `scripts/lint-patterns.allow` に理由付きで書く。**
  想定される正当な例外はノードのカテゴリ色と Viewer のガイド色
- 行高を 2 段に統一（`outliner` 22→24 / `media_bin` 28→24 /
  `palette` 26→24 / `attribute_spreadsheet` 22→24。
  `timeline` のプロパティ行 20 は `row.compact` として正しいので触らない）

### 完了条件

- `rg "rgb\(0x|hsla\(" crates --glob '!**/tokens.rs'` が
  **allow に載っているものだけ**を返す
- `lint-patterns.sh` が新しい規則で clean
- 行高の定数が 5 種から 2 種に減っている
- **`outliner` の「ラベルが 1 行に収まる」テストが 24px でも通る**
  （`outliner.rs:2304` が `ROW_HEIGHT` を assert しているので、
  22→24 で緩む側。**緩んだことを見落とさないよう、逆に
  「23px では溢れる」ことを要求するテストを足す**）

---

## Phase 3: 自前の 3 部品（`UIX-4`）

### 作業

- `Icon` / `Button` / `Tooltip` を `gpui-base` の primitive から作る
- **4 状態を全部持つ**: `hover` / `focus` / `press` / `disabled`
- **Tab 順と Enter / Space 起動を自分で持つ**（`tab_index` / `tab_stop` /
  `accessibility_id`）
- モーションは `feedback.in` / `feedback.out` の 2 種だけ
- gallery に**全部品 × 全状態のマトリクス**を並べる
- `ravel-app` の 130 箇所（`Icon` 58 / `Button` 51 / `Tooltip` 21）を
  差し替える

### 完了条件

- **状態機械がヘッドレスでテストされている** — 各部品について
  「`disabled` は `press` に遷移しない」「`focus` は `hover` と独立」
  「Escape で `Tooltip` が消える」
- **Tab 順が決定的にテストされている** — 宣言した順に `tab_index` が
  並ぶこと、`tab_stop` でない要素が飛ばされること
- **`Enter` と `Space` が click と同じ経路を通ることのテスト**
  （別経路だと片方だけ壊れる）
- モーションの時間軸がテストされている（`Animated` を偽の時計で進めて、
  120ms / 180ms で終わること）
- gallery を**実機で目視**し、4 状態が読み分けられることを確認
- `ravel-app` の 130 箇所が差し替わり、**既存テストが無改変で通る**

---

## Phase 4: 不変条件の違反を潰す（`UIX-6` / `UIX-7` / `UIX-8`）

**`issues/` の記述が正。** ここでは順序と、各 issue がどの不変条件に
対応するかだけを持つ。

### 順序（`UIX-6` → `UIX-7` → `UIX-8`）

`UIX-6`（選択と undo。**先にやる理由**: 他の修正の土台で、
ここが直らないと Phase 3 の部品差し替えで新しい選択バグが混ざったとき
切り分けられない）:

| 不変条件 | issue |
|---|---|
| 1 選択の所有権 | `MED-APP-05` / `MED-APP-06` |
| 2 選択の寿命 | `MED-APP-04` |
| 3 undo の粒度 | `MED-APP-07` / `LOW-APP-02` |
| 4 ドラッグの取り消し | `MED-APP-03` |

`UIX-7`（見え方と意味）:

| 不変条件 | issue |
|---|---|
| 5 どの幅でも | `MED-UI-07` |
| 6 死んだ操作 | `MED-APP-17` |
| 7 値の意味 | `MED-APP-19` / `MED-APP-20` / `MED-APP-29` / `MED-APP-30` |
| 9 派生キャッシュ | `MED-APP-08` |

`UIX-8`（設定の適用）: `MED-APP-10`。**ライトテーマとロケールが
同時に到達可能になる**（同じ設定適用経路なので 1 単位）。

### 完了条件

- 各 issue が `issues/closed/` に移り、`**解決済み**` 行を持つ
- **各修正に「その不変条件を破ると落ちる」テストがある。**
  「直った」を目視で済ませない
- `UIX-8` の完了後、**日本語ロケールとライトテーマが設定から選べる**

---

## Phase 5: 文書（`UIX-9`）

### 作業

- `docs/ui-impl-status.md`: 密度と部品の記述を実態に合わせる
- `docs/gpui-ui-guide.md`: 「部品を追加する」節を足し、
  `ravel-widgets` の gallery に登録するところまでを手順にする
- `docs/dev/`: 部品を追加するときのチェックリスト
  （4 状態・Tab 順・トークン・gallery への登録）

### 完了条件

- `mise run docs:check` が通る
- **`gpui-ui-guide.md` の手順どおりに部品を 1 つ足せることを、
  実際に足して確かめる**（手順書は使わないと腐る）

## 完了条件（計画レベル）

- 不変条件 12 個が文書化され、`ravel-review` から辿れる
- `ravel-widgets` が独立して build / test / 実行でき、
  **`gpui-component` に依存しない**
- 色・間隔・字送りのリテラルが `lint-patterns.sh` で禁止され、
  例外が allow に理由付きで列挙されている
- `Icon` / `Button` / `Tooltip` が自前で、**4 状態 + Tab 順 +
  Enter / Space がヘッドレスでテストされている**
- 不変条件 1〜9 を破っている open issue が全部 closed
- **ライトテーマと日本語ロケールが設定から到達できる**

## 未解決

- **gallery の目視を誰がやるか。** この環境の `screencapture` は TCC で
  拒否されるので、**エージェントは絵を確認できない**。実機の目視は
  人間の作業として計画に残す
- **`Input` を借り続けることの代価。** gpui-kit の `Input` は自前の
  部品とトークンが違うので、**Properties の入力欄だけ質感が揃わない**
  可能性がある。`UIX-4` の gallery で並べて判断し、揃わないなら
  `Input` の再実装を別単位として起票する
- **`category_color` の族をトークンにするか。** ノード種別の色は
  「機能色」なので階調トークンとは別の体系になる。`UIX-1` で決める
