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

- [ ] Viewメニューからパネルのトグルが動作する
- [ ] Viewメニューアイテムにチェックマークが反映される
- [ ] ワークスペースプリセット切替でDockAreaレイアウトが再構築される
- [ ] Workspaceメニューにアクティブプリセットのチェックマーク表示
- [ ] 既存テスト全パス
- [ ] アプリ起動→トグル→プリセット切替がクラッシュなし
