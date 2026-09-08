# UI コンポーネント層と UX 不変条件の計画

> **Status**: `UIX-0` 済み — 2026-09-07。`UIX-1` は `KIT-1` と衝突しないので
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
- **テーマは既に JSON で、既にホットリロードされる。**
  `assets/themes/ravel.json` が gpui-component の `.theme-schema.json`
  形式で light / dark 2 テーマ × 色 36 キー。`load_ravel_themes()` が
  themes ディレクトリの `*.json` を全部 `ThemeRegistry` に入れ、
  `ThemeRegistry::watch_dir` が編集を即反映し、設定ダイアログの
  `theme_field` が `sorted_themes()` から選ばせる。
  **足りないのは「ユーザーが置ける場所」だけ** — `themes_dir()` の候補は
  3 つとも**アプリ側**（`.app` バンドル内 / バイナリ隣 / ワークスペース）で、
  ユーザーテーマを置くには署名を壊してバンドルに書き込むしかない

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
  `Popover` / `DataTable` / `Accordion`。各 1 箇所）。gpui-kit のまま使う。
  **ただし `TitleBar` 以外は全部 `gpui-base` にある**
  （`gpui-kit-migration-plan.md` の「`gpui-base` の棚卸し」）。
  借り続けるのは**費用の判断**で、不可能だからではない
- **`Input` を借りるという判断は 2026-09-08 に撤回した。**
  「IME・テキスト選択・undo を自前で持つ費用が見合わない」を根拠に
  していたが、**根拠が事実と違った** —
  `gpui_component::Input`（1077 行）は **`gpui_base::InputBase` のラッパ**で
  （`use gpui_base::InputBase as BaseInput;`）、IME・選択・undo・masking は
  全部 `gpui-base` 側にある。component 側はスタイルを着せているだけで、
  `Button` / `Tooltip` と同じ作業だった。→ **`UIX-4b`** を切った
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
| トーストの登場と退場 — **不透明度だけ** | 何も無かった場所に現れるので手がかりが要る。**位置は動かさない**（右下に固定。詳細は「見た目の仕様」） |
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

### テーマスキーマは Ravel が持つ（gpui-component から離れる）

**決定: 2026-09-07。** 今の `assets/themes/ravel.json` は
gpui-component の `.theme-schema.json` に従っているが、`UIX-1` で
**Ravel 独自のスキーマに移る。**

理由: **間隔とモーションは gpui-component が持つ気の無い概念である。**
あちらのスキーマは色 36 キーと `font.size` / `radius` までで、
4px 刻みの間隔や `feedback.in` / `feedback.out` のような時間を
表す場所が無い。フォークにスキーマを足すと**上流追従のたびに
衝突する**（`KIT-0b` で 2211 コミット遅れが 9 コミットを無駄にしたのと
同じ費用の出方）。

**ただし gpui-component の `Theme` は生かし続ける。** 借りている部品
（`Input`、裾の 8 部品、`Root`）が `cx.theme()` を読むので、
**Ravel のスキーマを正として `ThemeConfig` を導出する**。方向を
逆にすると、Ravel 独自のトークンが gpui-component の型に入らない。

```
ユーザーの theme.json  →  RavelTheme（色・間隔・字送り・モーション）
                              ├→ ravel-widgets の部品が直接読む
                              └→ ThemeConfig を導出 → gpui-component の Theme
                                                        （借りている部品用）
```

移行の代価: 既存 `ravel.json` を書き換える。**ホットリロードと
設定ダイアログの選択経路は `ThemeRegistry` のままなので変わらない**
（`ThemeRegistry` に入れるのは導出後の `ThemeConfig`）。

## 見た目の仕様

**決定: 2026-09-08。** 部品ごとの見た目をここで詰め切った。`UIX-3` 以降の
実装が読む正であり、あとから部品を足すときの参照でもある。

### 状態の色は 10 トークンから導出する

**トークンを増やさない。** `Colors` は 10 のままで、状態は純関数で導く。
**`is_dark` の分岐を 1 つも持たないこと** — `foreground` が既にモードで
逆向きなので、そこへ混色すれば向きが自動で正しくなる。ユーザーが中間
グレーを `accent` に書いた themes でも壊れない。

| 導出するもの | 式 | light / dark |
|---|---|---|
| hover の面 | `accent` | `#E0E0E0` / `#282629` |
| press の面 | `accent.blend(foreground, 0.08)` | 暗く / 明るく |
| disabled の前景 | `foreground.opacity(0.38)` | |
| フォーカスリング | `primary` | `#5B6EE1` |
| 浮く面 | `background.blend(black, 0.03)` | `#F5F5F5` / `#101010` |
| 行の hover | `accent` | |
| 行の選択の面 | `primary.opacity(0.08)` | |
| 行の選択のバー | `primary` | |
| Slider / Progress の塗り | `primary.opacity(0.25)` | |
| スクロールバーのつまみ | `muted_foreground.opacity(0.40)` | |
| 同・hover | `muted_foreground.opacity(0.70)` | |
| タブ帯・非アクティブタブ | `accent` | |
| アクティブタブ | `background` | |
| Checkbox / Radio の塗り | `primary` | |
| 同・マーク | `background` | |

**導出が今の見た目を保つことは裏取り済み。** 浮く面の導出値は出荷
`ravel.json` の `popover.background` と一致し、行の 2 つは
`list.active.background`（α 8%）/ `list.active.border` と**完全に一致する**。

### 4 状態は既存機構に載る

**重ね順を自前で決めない。** `gpui-base` が契約として持っている
（`state_style.rs`）:

```
1. instance style（Styled のビルダ連鎖）
2. checked / pressed / selected / focused
3. disabled ← 常に最後
```

`hover` / `active` / `focus_visible` は **GPUI ネイティブ**。とくに
`focus_visible` は doc が「CSS の `:focus-visible` 相当。要素がフォーカス
され、**かつユーザーがキーボードで移動しているときだけ**適用される
（マウスクリックでは適用されない）」と明記しているので、**キーボードの
ときだけリングを出すために入力源を追跡する機構を書かない**。

### フォーカスリングは内側

**1px、`primary`、要素の境界の内側。** マウスのクリックでは出さない
（`focus_visible`）。

外側のリングは使わない。`ravel-app` / `ravel-dock` に `overflow_hidden` が
**24 箇所**あり、gpui-component の `focus_ring_style` の doc 自身が
「祖先が clip すると切れる」と書いている。内側なら切られず、要素の
サイズも動かない。

### 枠線を出す場所

| 枠線あり | 枠線なし |
|---|---|
| パネル境界 | Button（ghost も実体も） |
| 浮く面 | Tab |
| Input / NumberInput / Select | チップ |
| 選択中の行の左端（2px） | Icon |

ツールバーに 20px のボタンが 8 つ並ぶと、枠線があれば 9 本の縦線が見える。
面色だけなら hover した 1 つしか見えない。

### 寸法

| | compact | default |
|---|---|---|
| Button の高さ | 20px | 24px |
| Button の横 padding | 4px | 8px |
| Button の gap | 4px | 6px |
| Icon | 12px | 16px |

- **アイコンのみの Button は正方形**（20×20 / 24×24）
- **Button が自分の段からアイコンのサイズを決める。** 呼び出し側は書かない
  （明示指定の口は残す）
- `medium`（32px）は持たない。実測で**未使用**だった
- `scrub_input` と Properties のパラメータ行は **16px → 20px**
  （`row.compact`）。AE / Blender / Houdini のプロパティ行はどれも 20〜22px。
  **40 パラメータのパネルが 640px → 800px になる**（`UIX-5`）
- **`node_editor/painting.rs` の量子化のはしご `[8,12,16,24,32]` は
  クロームのアイコンには当たらない。** あれはズームで拡大するキャンバスの
  グリフ用で、アトラス保護のためのもの

### 部品ごと

**Slider / Progress** — **塗り面式、つまみ無し。** 行の高さをそのまま使い、
左から塗り、値のテキストを中央に重ねる。面のどこでもドラッグできる
（つまみを探さない）。**「つまめる」は hover で語る。**

**Checkbox / Radio** — 14px。未チェックは `border` の枠だけで面なし。
チェック済みは面を `primary` で塗り、マークを `background` 色で抜く。
Radio の点は 6px。**塗りが強いので、30 並んだリストでもオンの行を
一目で数えられる。**

**Tab** — アクティブタブの面色を**パネル本体と同じ**にし、タブ帯と
非アクティブタブは `accent`。タブと中身が繋がっていることが形で読めるので、
ドックの入れ子が深くてもどのタブがどの面のものか見失わない。

**行の選択とは別の語彙を使う。** タブは「どの面が前面か」、行は
「どれを選んだか」で、意味が違うから同じ表現にしない。

**スクロールバー** — **レーンの幅は 12px 固定**、つまみが 6px、hover で
10px。**幅を固定するのは不変条件 11（レイアウトを動かすな）のため** —
つまみだけ太るので中身は 1px も動かない。常時表示にするのは、
タイムラインやアウトライナで全体量が常に見えている方が設計作業に向くから。

**Tooltip** — `SHOW_DELAY` 500ms、`GRACE_PERIOD` 300ms。**`Motion` トークン
には入れない。** あれは「状態のフィードバックだけ」という定義を保つ。
出現遅延は意図の検出であって見た目ではない。速さを出したくなったら
**テーマではなくアプリ設定**に入れる（アクセシビリティの話）。

**通知（toast）** — **不透明度だけ**（120ms / 180ms）、位置は右下に固定。

**浮く面（Tooltip / Popover / menu / notification / dialog）** — **影なし。**
`background.blend(black, 0.03)` の面 + `border` の 1px。**高さは 1 段だけ。**
影を使わないので GPU のブラーパスが増えず、液晶で境界がぬるくならない。

### disabled が何に掛かるか

**「押せる」を語る塗りは消し、「状態」を語る塗りは 38% に落として残す。**

| 塗り | disabled で |
|---|---|
| hover / press の面 | 消す |
| Button の実体の面（primary / danger） | 消す |
| Checkbox のチェックの塗り | **38% で残す** |
| Radio の点 | **38% で残す** |
| Slider の塗り面 | **38% で残す** |
| Tab のアクティブの面 | **38% で残す** |
| テキストとアイコン | `foreground.opacity(0.38)` |

機械的に「面を消す」を当てると、**disabled かつチェック済みのチェック
ボックスが未チェックに見える** — あの塗りは状態そのものだから。

### color_picker

**残っている中で唯一「導出できない」部品。** `gpui-base` の doc が
「アプリがパレット・ポップアップ・レイアウト・**すべての視覚的判断**を
所有する」と明言していて、渡ってくるのは H / S / L / A の 4 スライダ状態と
hex 入力状態だけ。

ポップアップの中身:

```
┌──────────────┐┌─┐
│              ││▓│   彩度×明度の 2D 面 120×120
│       ●      ││▓│
│              ││▓│   色相帯 12px
└──────────────┘└─┘
│ A ▓▓▓▓▓▓▓▓▓▓  100% │  Slider（塗り面式）
│ #5B6EE1              │  hex 入力
```

描画手段は揃っている（**2D 面に自前の canvas は要らない**）:

| 要るもの | 手段 |
|---|---|
| 2D 面 | **div 2 枚。** 横に `白 → 色相`、上に `透明 → 黒`。各 2 停止で足りる |
| 色相帯 | `linear_gradient` は**停止点がちょうど 2 個**なので、**6 分割して積む** |
| 透明の下地 | **`pattern_slash(color, width, interval)` が組み込み。** 斜線、1 行 |

**スウォッチ（プロパティ行に出るトリガ）** — 16×16、角丸 2px、
`pattern_slash` の下地の上に色。**α が 1.0 未満のとき透けて見えることが
要件** — 「色が黒い」と「透明」の区別は合成では常に問題になる。

**キーボード** — Tab で 2D 面に入り（内側リング）、`←→` が彩度 ±1%、
`↑↓` が明度 ±1%、Shift で ±10%。そこから Tab で色相帯 → A → hex。
**マウスとキーボードが同じ値の道を通ること**（値を書く関数に両方が入る）。

### 矢印キーの文脈を 1 つ足す

**`←` / `→` は `playback.step_forward` / `step_backward` にグローバル
割り当て済み**（`assets/keybindings/default.toml`）で、その文脈は
`!Input && !PopupMenu && !AppMenuBar`（`workspace.rs:682`）。カラーピッカーは
`Popover` で `PopupMenu` ではなく、2D 面は `Input` でもないので、**2D 面に
フォーカスして `→` を押すと彩度が動くと同時に再生ヘッドも 1 フレーム進む。**

`workspace.rs` のコメントが自分で「`MED-APP-16` again, one context deeper
than where it was first fixed」と書いている穴の、3 段目である。

**「この要素が矢印を所有する」を意味する名前付きのキー文脈を定義し、
`yield_to_open_menus` の除外リストに入れる。** 付ける側は color_picker の
2D 面、矢印を取るなら Slider、`curve_editor` の点の微調整。

**グローバルの文脈に触るので単独の単位にする**（`UIX-10`）。

### 色の範囲は今のままにする（天井を記録）

色パラメータの**色空間は既に正しい** — `ColorSpace::DISPLAY.from_linear` /
`.to_linear` で linear ↔ display を往復していて、`composition_form.rs` と
`properties.rs` が対で持っている。

**範囲は正しくない。** `Hsla` と `#RRGGBB` は 0..1 なので、**1.0 を超える色
パラメータは往復できない**。ただし `Color` パラメータを使うノードは
`constant` / `style` / `rasterize` の 3 つだけで、exposure / emission 系の
ノードは存在しないので、**現状のバグではなく天井**。HDR の色を持つノードを
足すときにここへ戻る。

### menu は借り続ける

**`gpui-base` に `menu.rs` が無い**（`popup.rs` / `popover.rs` のみ）。
無スタイル層が存在しない唯一の部品で、7 箇所使っていてアプリメニューバーの
OS 連携も含む。

**`UIX` の「gpui-component ゼロ」の範囲を `Icon` / `Button` / `Tooltip` /
`Input` に限り、menu は `KIT-6` に寄せる。** 影なし・高さ 1 段の浮く面だと
**サブメニューが親メニューの上で 1 枚の面に見える**問題があるので、menu を
自前にするときはそこを決め直す。

---

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
| `Input` | 20 | **`UIX-4b` で `gpui-base` に載せ替える**（`InputBase` が挙動を持っているので、着せるだけ。2026-09-08 に「借りる」から変更） |
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
| `UIX-0` | 不変条件を **`.agents/rules/ux.md`** に文書化し、`ravel-review` の検査手順に入れる | — |
| `UIX-1` | **`ravel-widgets` クレートを作り**（`gpui` + `serde` のみ）、**Ravel 独自のテーマスキーマ**を定義する。gpui-component の `ThemeConfig` の導出は `ravel-app` 側。**まだ配線しない** | — |
| `UIX-2` | `ravel-widgets` に `gpui-base` を足し、既存 6189 行を移設。`examples/gallery` を作る | `KIT-1` / `UIX-1` |
| `UIX-3` | `tokens.rs` を配線し、ハードコード 24 箇所を潰す。`lint-patterns.sh` にリテラル禁止を追加 | `UIX-1` / `UIX-2` |
| `UIX-4` | `Icon` / `Button` / `Tooltip` を `gpui-base` から自前で作る。4 状態 + Tab 順 + Enter / Space | `UIX-2` |
| `UIX-4b` | **`Input`** を `gpui-base` に載せ替え、`scrub_input.rs` を `ravel-widgets` へ移す。`gpui_base::number_input` と比べてどちらを使うか決める | `UIX-4` |
| `UIX-5` | 行高を 2 段に統一（`row.compact` 20 / `row.default` 24） | `UIX-3` |
| `UIX-6` | **不変条件 1〜4 の違反を潰す**（選択の所有権・寿命、undo の粒度、ドラッグの取り消し） | `UIX-0` |
| `UIX-7` | **不変条件 5〜9 の違反を潰す**（狭い幅、死んだ操作、値の意味、設定の適用、派生キャッシュ） | `UIX-0` |
| `UIX-8` | **ユーザーテーマディレクトリ**を足す（`themes_dir()` を複数候補に、ユーザー側が勝つ、**watch を自前に持つ**、**`assets/themes/ravel.schema.json` を同梱して `$schema` で指す**、書き方の文書） | `UIX-1` |
| `UIX-9` | 文書更新（`ui-impl-status.md` の密度・部品の記述、`gpui-ui-guide.md` の「部品を追加する」節） | `UIX-4`〜`UIX-8` |
| `UIX-10` | **矢印キーを所有する要素のキー文脈**を定義し、グローバルバインドの除外リストに入れる（`←` / `→` が `playback.step_forward` と衝突する。`MED-APP-16` の 3 段目） | — |
| `UIX-11` | **`color_picker` を自前で作る**（2D 彩度面 + 色相帯 + A + hex、`pattern_slash` のスウォッチ） | `UIX-4` / `UIX-10` |

**`UIX-0` と `UIX-1` はパネルを触らないので `KIT-1` と並行できる。**
`UIX-2` 以降は `gpui-base` がツリーに入るまで書けない。

---

## Phase 0: 不変条件とトークンの確定（`UIX-0` / `UIX-1`）

`KIT-1` と衝突しない。文書と型定義だけ。

### 作業

- **`.agents/rules/ux.md`** に 12 個を書く。**各項目に「破ったときに
  どう見えるか」**を添える（抽象的な原則だけだと検査に使えない）。
  `docs/dev/` ではなく `.agents/rules/` に置くのは、`docs/dev/README.md` の
  分類で不変条件が「規範」に当たるためで、**`paths` frontmatter で
  パネルを触る diff に自動的に読み込まれる**のが実利
- `ravel-review` スキルの検査手順に「UX 不変条件」の節を足す
- **Ravel 独自のテーマスキーマを定義する**（決定: 2026-09-07。理由は下記）
- 色は既存の 10 種 + `category_color` の族から始め、増やすのは実際に
  要ったときだけ（先回りして 40 色作らない）
- 間隔は 4px 刻み、行高は上の 2 段、字送りは既存の
  `theme().font_family` / `mono_font_family` に段を足す形
- モーションは `Motion` 2 種だけ（`feedback.in` 120ms / `feedback.out` 180ms）
- **`ThemeConfig` の導出を書く** — 借りている部品（`Input` と裾の 8 部品、
  `Root`）は `cx.theme()` を読むので、gpui-component の `Theme` に
  値を供給し続ける必要がある。**Ravel のスキーマが正で、そこから
  `ThemeConfig` を作る**（逆ではない）
- 既存 `assets/themes/ravel.json` の移行を書く

### 完了条件

- 不変条件 12 個が `.agents/rules/ux.md` にあり、各項目に違反時の見え方が
  書かれている
- `AGENTS.md` のルール一覧と `.agents/rules/gpui.md` から辿れる
- `ravel-review` の手順から辿れる
- トークンの型が `mise run check` を通る（値は入っているが誰も読まない状態）
- **`mise run docs:check` が通る**

---

## Phase 1: 部品層の器（`UIX-2`）

**`KIT-1` 待ち。**

### 作業

- `crates/ravel-widgets` に `gpui-base` / `ravel-i18n` を足す
  （クレート自体は `UIX-1` が `gpui` + `serde` で作っている）
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

`UIX-8` はここには入らない — **設定の適用経路は既に動いている**
（`app_settings::install` → `apply(Changed::ALL)` が locale / appearance /
cache を配線し、ロケールもテーマも設定ダイアログから選べる）。
`MED-APP-10` に残っているのは **autosave / proxy / OCIO** の 3 つで、
これは UI の不変条件ではないのでこの計画の範囲外。

### 完了条件

- 各 issue が `issues/closed/` に移り、`**解決済み**` 行を持つ
- **各修正に「その不変条件を破ると落ちる」テストがある。**
  「直った」を目視で済ませない
- `UIX-8` の完了後、**ユーザーが自分のテーマ JSON を置いて選べる**
  （アプリバンドルに書き込まずに）

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
- **順序の穴を 1 つ直した**（2026-09-07）。当初 `UIX-1` はトークンを
  `ravel-widgets` に置く前提だったが、そのクレートを作るのは `UIX-2`
  （`KIT-1` 待ち）だったので書く場所が無かった。スキーマに要るのは
  `gpui`（`Hsla`）と `serde` だけで **`gpui-base` は要らない**ので、
  `UIX-1` がクレートを作る形にした
- **`$schema` は `UIX-1` で外したまま。`UIX-8` が Ravel の schema を同梱して
  指し直す。** 今指せる先が無い（gpui-component のものは嘘、Ravel の URL は
  存在しない）ので外した。**ユーザーが手書きテーマを書くときにエディタ補完が
  効くかどうかは体験そのもの**なので、`UIX-8` は
  `assets/themes/ravel.schema.json` を**同梱**し（URL ではなく相対パスで
  指す。オフラインでも効く）、`ThemeSpec` の形と一致することをテストで
  固定する。**手書きの schema は本体と乖離する**ので、`schemars` で
  `ThemeSpec` から生成して差分をテストするのが安い
  （`schemars` は既にツリーにある — gpui が使っている）
- **ホットリロードが Ravel のスキーマを通らない。`UIX-8` が閉じる。**
  `ThemeRegistry::watch_dir` の `on_load` コールバックは
  最初の `reload_themes` の直後に**1 回だけ**呼ばれ、その後の再読み込みは
  `_watch_themes_dir` が張った監視の中で起きる（`registry.rs:105-115`）。
  **後続の再読み込みにフックは無い**ので、借りたまま導出を挟むことは
  できない。**今は到達不能**（ユーザーがテーマを置ける場所が存在せず、
  同梱 `ravel.json` は Ravel が模型化した 10 色を全部書いているので
  導出結果と一致する）。**穴を露出させる変更と閉じる変更が同じ
  `UIX-8`** なので、あの単位は `notify` で watch を自前に持つこと
  （`notify` は既に依存にある）
- **`ThemeRegistry::watch_dir` が 1 ディレクトリしか受けない。**
  `UIX-8` は 2 ディレクトリ（同梱とユーザー）を監視する必要があるので、
  受けないなら `notify` で自前に張るか、gpui-kit フォークに複数
  ディレクトリ対応を入れる。**どちらになるかは `UIX-8` の着手時に測る**
- **同名衝突はユーザー側が勝つ。** `ThemeRegistry` は先に読んだものを
  残す仕様なので、ユーザーディレクトリを先に読む
