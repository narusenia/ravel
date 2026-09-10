# 部品（ウィジェット）を追加する

> 索引: [`README.md`](README.md)

`crates/ravel-widgets` に Ravel 自前の部品を 1 つ足す手順。設計の意図は
[`../implementation/ui-component-layer-plan.md`](../implementation/ui-component-layer-plan.md)
の「見た目の仕様」、守るべきことは
[`.agents/rules/ux.md`](../../.agents/rules/ux.md) の 12 個の不変条件、
GPUI のパターンは [`../gpui-ui-guide.md`](../gpui-ui-guide.md#部品を追加する)。

## チェックリスト

- [ ] `gpui-base` に対応する無スタイル primitive があるか確認する
      （挙動を借りられるなら自分で書かない）
- [ ] `crates/ravel-widgets/src/<name>.rs` を作り、`lib.rs` に `pub mod` と
      `pub use` を足す
- [ ] **見た目を返す純関数**（`<name>_layers`）を作り、`render` には判定を
      書かない
- [ ] **4 状態**（rest / hover / press / disabled）を持たせる。
      disabled では hover / press を**インストールしない**
- [ ] 寸法は `Density::metrics(theme)` から、色は `Colors` の**導出メソッド**から
      取る（`is_dark` で分岐しない。不変条件 12）
- [ ] **Tab 順**（`tab_index` / `tab_stop`）と **Enter / Space** を通す。
      pointer と鍵盤は**同一の経路**（不変条件 10）
- [ ] 公開するハンドラは `Fn(&T, &mut Window, &mut App)` の 3 引数
      （`cx.listener` が作れる形。primitive の 4 引数をそのまま出さない）
- [ ] マークをアイコンで描くなら `UiIcon` に足す（`ALL` の要素数も更新）
- [ ] フォーカスリングは要素の**内側**の 1px（`Button` は `focus_visible`、
      クリックして入る面は `focus`）
- [ ] `examples/gallery.rs` の `SECTIONS` に 1 行足す
- [ ] 純関数のヘッドレステスト + 挙動が窓を要るなら `#[gpui::test]`
- [ ] 借用していた部品を差し替えたなら、呼び出し側の
      `gpui_component::…` の import が消えたことを確認する
- [ ] [`../ui-impl-status.md`](../ui-impl-status.md) と
      [`../../AGENTS.md`](../../AGENTS.md) の `ravel-widgets` の説明を直す
- [ ] `mise run check` と `mise run docs:check`

**忘れやすいもの**: `ravel-widgets` に `gpui-component` を足さないこと。
テーマスキーマは Ravel 側が正で、gpui-component の `ThemeConfig` は
`ravel-app` が*導出*する。逆向きの依存は入れない（`lib.rs` の crate doc）。

## 1. 借りられる挙動を探す

`gpui-base` の primitive（`Button` / `Checkbox` / `InputBase` /
`ColorPickerState` …）は無スタイルで、フォーカス・Tab stop・
アクセシビリティロール・**pointer と Enter / Space の単一の活性化経路**を
持っている。あるなら着せるだけにする。

- 状態型（`CheckboxState` / `InputState` …）は `gpui-base` のものを
  そのまま再エクスポートする。Ravel 側で似た enum を作らない
- **遷移規則も primitive のもの。** 活性化がどの状態に着地するかは
  primitive が決めているので、転送するだけにする（判定関数は private で、
  写せば規則の出どころが 2 つになる）。Ravel 側のテストはその規則を
  **窓越しに読む**（`every_state_activates_to_the_one_the_primitive_names`）
- 公開するハンドラは `Fn(&T, &mut Window, &mut App)`。primitive の
  `Fn(T, &ClickEvent, …)` を素通しすると、**呼び出し側が `cx.listener` を
  使えなくなる**。`ClickEvent` は要求する呼び出し側が出るまで落とす
- 活性化のハンドラは 1 つだけ受ける（`on_click` / `on_change`）。
  **`on_key_down` を足さない** — 2 経路作ると片方が腐る（不変条件 10）
- primitive が持つ状態は、今の呼び出し側が使っていなくても**描けるように
  しておく**（`Checkbox` の `Indeterminate` は Properties の複数選択で要る）

## 2. 見た目は純関数に出す

`render` から判定を読めるようにしない。状態を受けて**層**を返す関数を作り、
`render` はそれを当てるだけにする。実物は `button_layers` /
`input_layers` / `checkbox_layers` / `frame_is_focused`。

```rust
pub fn checkbox_layers(
    state: CheckboxState,
    disabled: bool,
    colors: &Colors,
) -> CheckboxLayers { … }
```

こうする理由は 2 つ:

- **窓なしでテストできる。** 4 状態の見た目は `#[test]` で固定でき、
  `#[gpui::test]` が要るのは「フォーカスと入力源」だけに減る
- **gallery が同じ関数を呼べる。** hover を再現しなくても 4 状態を
  1 画面に並べられる（`button_section` / `checkbox_section` の swatch 列）

## 3. 寸法・色・字はトークンから

| 要るもの | 出どころ |
|---|---|
| 高さ・横 padding・gap・アイコン・字の大きさ | `Density::metrics(theme)` |
| 色 | `Colors` の導出メソッド（`hover_surface()` / `pressed_surface()` / `disabled_foreground()` / `focus_ring()` / `raised_surface()` / `selected_surface()` / `toward_foreground()` / `readable_on()`） |
| 角丸 | `theme.radius` |
| モーション | `theme.motion`（状態のフィードバックだけ。不変条件 11） |

- **行高は 2 段だけ**（`row.compact` 20 / `row.default` 24）。3 段目を
  発明しない。段で表せない意味なら、その意味を先に言う（不変条件 12）
- **`is_dark` で分岐しない。** `foreground` が既にモードで逆向きなので、
  `toward_foreground` に混ぜれば向きは自動で正しくなる
- **計画書が名指しした色は計画書が正。** 導出は決まっていないところを
  埋めるもの。Checkbox のマークは「`background` で抜く」と決まっており、
  `readable_on(primary)` はライトパレットでは `foreground` を選ぶので
  （4.8:1 対 4.4:1）、そこを導出に任せるとライトだけ黒いチェックになる。
  導出はその色が働かなくなる派生状態（38% の面の上）に使う
- **入れ子の部品の hover / press は、フォーカスを持つ根に置く。** GPUI の
  `hover` は書いた要素にしか掛からず、`gpui_base::CheckboxIndicator` は
  `InteractiveElement` を実装していないので子を親のホバーで動かせない。
  状態を語る子の塗りと、操作を語る根の面を分ける
- 部品固有で**トークンに無い寸法**（14px の Checkbox、12px のアイコン）は
  `tokens.rs` の名前付き定数にする。painter の中に数字を置かない
- `gpui::ColorExt::blend` は使わない。`tokens::mix` を使う
  （理由は `mix` の doc）

## 4. 4 状態と disabled

```rust
hover: (!disabled).then(|| …),
pressed: (!disabled).then(|| …),
focus_ring: (!disabled).then(|| …),
```

**disabled のとき hover / press を「上から消す」のではなく、
最初からインストールしない。** GPUI は `hover` を要素スタイルの*後*に
解決するので、インストールしたままだと disabled でもポインタで光る。

disabled で消すものと残すものは分かれる（計画書の「disabled が何に掛かるか」）:

- **消す**: hover / press の面、Button の実体の面
- **38% で残す**: Checkbox のチェックの塗り、Radio の点、Slider の塗り面、
  Tab のアクティブの面 — これらは「押せる」ではなく**状態**を語る塗りなので、
  機械的に消すと disabled かつチェック済みが未チェックと同じ絵になる
- テキストとアイコンは `disabled_foreground()`（= `foreground` の 38%）

## 5. gallery に登録する

`crates/ravel-widgets/examples/gallery.rs` の `SECTIONS` に 1 行と、
節を描く `fn` を 1 つ。窓・テーマトグル・レイアウトは触らない。

```rust
("checkbox · 三状態と 4 状態", checkbox_section),
```

- **生きた部品**（実際に押せるもの）と、**純関数から作った swatch 列**の
  両方を出す。後者が「4 状態が読み分けられる」の確認になる
- 節の `fn` は `cx` を受け取らないので状態を持てない。生きた部品は
  *制御された*状態を並べる（トグルの正しさは単体テストの担当）
- アイコンのパスは gallery の `gpui_kit_assets::Assets` と、アプリの
  `RavelAssets` の**両方で解決する**必要がある
- 両パレットで見ること。テーマトグルは
  `ravel_widgets::set_active_tokens` も呼ぶので、節が自分のトークンと
  食い違うことはない
- `cargo run -p ravel-widgets --example gallery` で開く。
  `--release` は速いが、**debug でも実用になる**（`Cargo.toml` の
  `[profile.dev.package]` が `gpui-ce` と `taffy` を上げている。
  理由と実測は gallery の module doc）

## 6. テスト

| 何を | どこで |
|---|---|
| 4 状態の見た目、状態から見た目への写像 | `<name>.rs` の `#[test]`（純関数を呼ぶ。窓不要） |
| フォーカス・Tab 順・Enter / Space・入力源 | 同じファイルの `#[gpui::test]` |

- 「disabled が hover / press / ring に到達しない」は**両パレット・全 variant**
  で回す（`a_disabled_button_reaches_no_hover_press_or_ring` が雛形）
- primitive 側が既にテストしている挙動（`gpui-base` の活性化・a11y）は
  **書き写さない**。Ravel 側のテストは「Ravel が決めたこと」を固定する

## 7. 呼び出し側を差し替える

借用部品を置き換えたときは、同じ変更で呼び出し側も移す。

- `gpui_component::…` の import が**消えている**ことを grep で確認する
  （語境界で数える。部分文字列 grep は箇所数を水増しする）
- 挙動を変えない: トグル、ラベルのクリック、disabled、undo の粒度
- 呼び出し側は**トークン以外の色・寸法を書かない**。書いていたら消す

## 8. 文書

- [`../ui-impl-status.md`](../ui-impl-status.md) の該当行（部品名・ファイル
  パス）を直す。**借り続けている部品を自前と書かない**
- [`../../AGENTS.md`](../../AGENTS.md) のリポジトリマップの `ravel-widgets`
  の記述に足す
- 手順が足りなかったら**このページと
  [`../gpui-ui-guide.md`](../gpui-ui-guide.md#部品を追加する) を直す**。
  手順書は使わないと腐る
