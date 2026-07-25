# TASK-006a: GPUIパネルトグル + プリセット切替
- **マイルストーン**: MS1 Foundation
- **関連要件**: REQ-UI-001, REQ-UI-005
- **規模**: M
- **依存タスク**: TASK-006 (部分完了)

## 概要

GPUI実結線の第2弾。AppShellの状態変更（パネルVisibility、プリセット切替）をDockAreaに反映する。View/Workspaceメニューのチェック状態もライブ更新。

## 実装ステップ

1. **RavelWorkspaceのEntity化** — アクションハンドラからshellを更新できるようにRavelWorkspaceをGPUI Entityとして管理
2. **パネルトグル連動** — ViewToggle系アクション → AppShell.handle_command → PanelVisibility更新 → DockAreaでパネルshow/hide
3. **メニューライブ更新** — 状態変更後に`cx.set_menus(build_menus(&shell))`を再呼出し。Viewメニューにチェックマーク反映
4. **プリセット切替** — Workspace系アクション → AppShell.switch_preset → DockAreaレイアウト再構築
5. **プリセットメニューチェック** — Workspaceメニューでアクティブプリセットにチェックマーク

## 完了条件

- [x] Viewメニューからパネルのトグルが動作する
- [x] Viewメニューアイテムにチェックマークが反映される ※ヘッドレスモデル層で正しく追跡・テスト済。GPUIネイティブメニューにchecked variant未実装のため実描画は未反映（フレームワーク制約）
- [x] ワークスペースプリセット切替でDockAreaレイアウトが再構築される
- [x] Workspaceメニューにアクティブプリセットのチェックマーク表示 ※同上（ヘッドレスモデル正常、GPUI描画側制約）
- [x] 既存テスト全パス（222 pass, 0 fail, 1 ignored）
- [x] アプリ起動→トグル→プリセット切替がクラッシュなし

> **実装メモ（2026-06-24）**:
> - Outliner / Dopesheet パネルを追加し、4プリセット（Edit/Node/Color/Motion）のレイアウトを再設計。
> - `AppShell` → `RavelWorkspace` (GPUI Entity) → `DockArea` の連動を実装。
> - メニューチェックマーク: `ravel_ui::menu` モデル層で `check: Option<bool>` を正しく管理し53テストで検証済。
>   GPUI 0.2.2 の `gpui::MenuItem::Action` に checked フィールドが存在しないため、ネイティブメニュー描画では未反映。
>   将来的に `gpui_component::PopoverMenu` によるカスタムメニュー描画で対応予定。
> - タブグルーピング（Outliner/MediaBin、Dopesheet/CurveEditor）: `LayoutNode` に `Tab` variant 未実装のため、
>   プリセットレイアウトでは片方のパネルのみ配置。EditプリセットのMediaBinは未配置（コメントで aspirational 記述のみ）。
>   `LayoutNode::Tabs` variant 追加は後続タスクで対応。
> - `rebuild_layout` はトグル/プリセット切替の両方でDockAreaを再生成。パネルトグル時のインプレース更新は将来最適化候補。
> - `ViewToggleOutliner` / `ViewToggleDopesheet` のデフォルトキーバインド未割当（メニューのみ操作可能）。
