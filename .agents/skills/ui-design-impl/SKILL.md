---
name: ui-design-impl
description: >-
  Ravel の UI（パネル・ウィジェット・アイコン）を設計・実装するときの手順。
  3層構造（ravel-core / ravel-ui / ravel-app）への切り分け、gpui-component
  ウィジェット選定、テーマカラー、t! による i18n、Lucide アイコンの
  オンデマンド vendoring と RavelIcon 登録までを一気通貫で扱う。
  トリガー: "/ui-design-impl"、新しいパネル・ウィジェット・アイコンの追加、
  「UIを作って」「アイコンを追加して」（Ravel リポジトリ内）。
---

# ui-design-impl

Ravel の UI 実装ワークフロー。着手前に必ず読むもの:

- `.agents/rules/gpui.md`（render 純粋性・focus 所有権・Command 経路）
- `docs/gpui-ui-guide.md`（canvas 描画・テーマ・イベントの実践知）
- `docs/agent-api-reference.md`（ravel-ui / ravel-app の API マップ）
- ウィジェット選定は `gpui-component` スキル、GPUI 基礎 API は `gpui-ce` スキル

## 1. 層の切り分け

| 置く場所 | 内容 |
|---------|------|
| `ravel-core` | ドメイン型・純ロジック（UI 依存禁止） |
| `ravel-ui` | ヘッドレス状態（選択・スクロール・展開等）+ ユニットテスト |
| `ravel-app` | GPUI ビュー（`Panel` 実装、canvas、イベント） |

新パネルは `PanelKind` 追加 → `panel_for_kind()` と `register_panels()` の
**両方**に concrete パネルを登録（片方だけだと reattach で Placeholder に戻る）。
コンストラクタは `new(window, cx)`、focus は `track_panel_focus()`。

## 2. 表示テキストと色

- ユーザー可視文字列は必ず `t!` / locale キー経由。
  `assets/locales/en.toml` と `ja.toml` の両方に追加する。
  ヘッドレス層（ravel-ui）はキーを発行し、ravel-app が translate する
  （`properties.section.*` 方式）。
- 色はハードコードせず `cx.theme().colors.*`（`ActiveTheme` import）。

## 3. アイコン

方針: **使う分だけ vendoring**。Lucide 全部盛りは禁止。
gpui-component ウィジェット内蔵アイコン（chevron 等）は
`gpui-component-assets` フォールバックが配信するので何もしなくてよい。

新しいアイコンが必要になったら:

1. https://lucide.dev で名前を確認し、**pinned バージョン**でダウンロード:

   ```bash
   V=0.462.0   # assets/icons/ 内の既存アイコンとバージョンを揃える
   curl -sf "https://unpkg.com/lucide-static@$V/icons/<name>.svg" \
     -o assets/icons/<name>.svg
   ```

   （ISC ライセンスは `assets/icons/LICENSE` に配置済み。バージョンを
   上げる場合は全アイコンを同時に更新し LICENSE も差し替える）

2. `crates/ravel-app/src/assets.rs` の `RavelIcon` enum に variant を追加し、
   `path()` にマッピングを追加。

3. 使用側: `gpui_component::Icon::new(RavelIcon::X).text_color(color)`。
   パネルタブは `panels::tab_title()` ヘルパーが icon+label を統一描画する。

4. `cargo test -p ravel-app assets` — `every_panel_icon_is_embedded` 等の
   埋め込み検証テストが通ること。enum に追加したのに SVG を置き忘れると
   ここで落ちる。

アセット配信の仕組み: `RavelAssets`（rust-embed、`assets/icons/**/*.svg`）
→ miss 時 `gpui_component_assets::Assets` へフォールバック。
`main.rs` の `.with_assets(RavelAssets)` で登録済み。

## 4. 検証

- `mise run check`（fmt + pattern lint + clippy -D warnings + tests）
- 見た目の変更は `cargo run` で起動確認（テーマ両対応を意識）
- PR 前に `ravel-review` スキル（render 純粋性等の文脈依存チェック + gate）
