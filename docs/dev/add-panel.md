# パネルを追加する

> 索引: [`README.md`](README.md)

新しいパネルを 1 枚足す手順。GPUI のパターンは
[`../gpui-ui-guide.md`](../gpui-ui-guide.md)、規約は
[`.agents/rules/gpui.md`](../../.agents/rules/gpui.md)。

## チェックリスト

- [ ] `crates/ravel-ui/src/panel.rs` の `PanelKind` に variant を追加
      （`ALL` の配列長も更新する）
- [ ] `assets/locales/en.toml` と `ja.toml` にパネル名のキーを追加
- [ ] ヘッドレス状態が必要なら `crates/ravel-ui/src/panels/` に置く
- [ ] GPUI パネルを `crates/ravel-app/src/panels/` に実装する
- [ ] `crates/ravel-app/src/panels/mod.rs` の `build_panel_view`（**単一の
      パネルファクトリ**）の `match` に生成を追加
- [ ] 表示トグルのコマンドを足す（[`add-command.md`](add-command.md)）
- [ ] 既定で配置するなら `assets/workspaces/*.toml` を更新
- [ ] `mise run check`

**忘れやすいもの**: ロケールキーは `PanelKind::ALL` を走査するテスト
（`ravel-ui/src/lib.rs` の `all_panel_label_keys_in_catalog`）が強制する。
en / ja のどちらかを忘れるとテストが落ちる。

## 1. `PanelKind` に足す

`crates/ravel-ui/src/panel.rs`。ヘッドレス側の識別子で、`panel_id()`
（永続化とプリセットで使う文字列）と `label_key()`（ロケールキー）を持つ。

- `PanelKind::ALL` は固定長配列なので**要素数の更新を忘れるとコンパイルエラー**
  になる（これは good failure）
- `panel_id()` はワークスペースプリセット（`assets/workspaces/*.toml` の
  `kind = "..."`）とレイアウト永続化に載る。**後から変えると保存済み
  レイアウトが読めなくなる**

## 2. ロケールを足す

```toml
# assets/locales/en.toml
[panel.my_panel]
_self = "My Panel"
empty = "Nothing selected"
```

サブテーブルの `_self` が `panel.my_panel` として引かれる規約
（[`add-locale.md`](add-locale.md)）。

## 3. ヘッドレス状態を分ける

パネルの状態とロジックのうち、**GPUI に依存しないもの**は
`crates/ravel-ui/src/panels/` に置く。理由は 2 つ:

- GPUI 無しで単体テストできる（既存パネルの状態遷移テストはすべてこの層）
- 描画と入力だけを `ravel-app` に残せる

例: Timeline のズーム・スクロール計算（`ravel-ui/src/panels/timeline.rs` の
`zoom_at` / `x_to_frame`）は headless、canvas 描画とマウス処理は `ravel-app`。

## 4. GPUI パネルを実装する

`crates/ravel-app/src/panels/` に `Render` を実装したエンティティを置く。
既存パネル（`media_bin.rs` が最も小さい）を雛形にする。

守るべき不変条件（`.agents/rules/gpui.md`）:

- **`render()` を純粋に保つ。** コマンド送出、フォーカス変更、状態変更を
  入れない。評価要求も出さない
- フォーカスは `track_focus` で保持し、子の入力からフォーカスを奪い返さない
- `key_context` を設定し、パネル固有ショートカットはそのコンテキストに束縛する
  （生の `on_key_down` で修飾キーを見るのは、テキスト入力や一時的なドラッグ
  モードのような本当に低レベルな入力に限る）
- 選択などの共有状態は Global を読む（`LayerSelection` / `CanvasSelection` /
  `ActiveComposition` / `MediaSelection`）。**パネル内部に第 2 の選択状態を
  持たない**
- コンポーネントのイベントは `EventEmitter` + `Subscription`。`Subscription` は
  observer の寿命だけ保持する
- 変更後は `cx.notify()`。ただし**変化していないときに notify しない**
  （canvas 描画のパネルは 1 回の notify が全面再描画になる）

## 5. 生成を登録する

`crates/ravel-app/src/panels/mod.rs` の `build_panel_view` に足す。**パネル
ビューが生成される唯一の場所**（旧 `panel_for_kind` / `register_panels` の
二重登録は DOCK-8 で統合済み）。

```rust
PanelKind::MyPanel => cx
    .new(|cx| my_panel::MyPanel::new(instance, window, cx))
    .into(),
```

- 戻り値は `AnyView`。`PanelViews`（`PanelInstanceId` キーのレジストリ）が
  キャッシュし、ravel-dock の `PaneContent` として窓に供給する
- コンストラクタは第 1 引数に `PanelInstanceId` を取り、
  `track_panel_focus(instance, …)` に渡す。**同一パネルが複数開かれる**ので、
  フォーカスもタブ表示もインスタンス単位
- `match` に足さないと `PlaceholderPanel` にフォールバックする（プレースホルダが
  出るのは仕様。未実装パネルはこの状態）
- 追加のドック側配線は無い。タブ・分割・detach はレイアウトモデルが扱う

## 6. 表示トグルと配置

- 表示トグルは `CommandId::ViewToggle*` を足し、`ravel-ui` の
  `AppShell::handle_command` の `match` に `toggle_panel(PanelKind::MyPanel)`
  を足す（[`add-command.md`](add-command.md)）
- `PanelKind::default_slot()` を設定する。プリセットが配置していないパネルは
  トグル時にこのスロットへ挿入される
- 既定で配置するなら `assets/workspaces/*.toml` の `layout` に `area` を足す
  （[`../specifications/ui/workspaces.md`](../specifications/ui/workspaces.md)）

## 7. 仕様と実装状況を書く

- 「こう動くべき」は `docs/specifications/ui/<panel>.md` に足し、
  [`../specifications/ui-spec.md`](../specifications/ui-spec.md) の索引と
  パネル一覧を更新する
- 実装済み範囲は [`../ui-impl-status.md`](../ui-impl-status.md) に書く
- **未実装の項目を実装済みとして書かない**
  （[`.agents/rules/documentation.md`](../../.agents/rules/documentation.md)）

## テスト

- 状態遷移は `ravel-ui` の単体テスト
- GPUI 統合テストは**フォーカス・Action 伝播・入力経路・描画に依存する挙動だけ**
  に限る（[`testing.md`](testing.md)）
