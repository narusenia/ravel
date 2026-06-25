# タスク仕様

## 目的

GPUI実結線の第2弾。AppShellの状態変更（パネルVisibility、プリセット切替）をDockAreaに反映する。View/Workspaceメニューのチェック状態もライブ更新。

## 要件

- [x] RavelWorkspaceのEntity化（アクションハンドラからshell更新可能に）
- [x] ViewToggle系アクション → AppShell.handle_command → PanelVisibility更新 → DockAreaでパネルshow/hide
- [x] 状態変更後に`cx.set_menus(build_menus(&shell))`再呼出し、Viewメニューにチェックマーク反映
- [x] Workspace系アクション → AppShell.switch_preset → DockAreaレイアウト再構築
- [x] Workspaceメニューでアクティブプリセットにチェックマーク

## 受け入れ基準

- Viewメニューからパネルのトグルが動作する
- ワークスペースプリセット切替でDockAreaレイアウトが再構築される
- 既存テスト全パス
- アプリ起動→トグル→プリセット切替がクラッシュなし

## 参考情報

- docs/implementation/tasks/TASK-006a.md
- docs/specifications/ui-spec.md
- REQ-UI-001, REQ-UI-005
- 依存: TASK-006（GPUIアプリケーションシェル）
