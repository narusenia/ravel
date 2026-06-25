# タスク仕様

## 目的

GPUI実結線の第3弾。パネルを別ウィンドウにデタッチ/復帰する機能と、Windows CIビルド対応。

## 要件

- [x] PanelDetach(Cmd+Shift+D)でフォーカス中のパネルをDockAreaから除去→新規ウィンドウで表示
- [x] デタッチウィンドウclose時 or PanelReattach(Cmd+Shift+R)でメインウィンドウのDockAreaに復帰
- [x] AppShell.windows()のdetach/reattach状態と同期
- [x] Windows CIビルドパス確認（gpui 0.2.2 D3D11対応済み）

## 受け入れ基準

- Cmd+Shift+Dでフォーカスパネルが別ウィンドウに表示される
- デタッチウィンドウclose時にパネルがメインウィンドウに復帰
- Cmd+Shift+Rでキーボードから復帰可能
- WindowManagerの状態がdetach/reattachサイクルで整合
- Windows CIビルドがパス
- 既存テスト全パス（235テスト pass、clippy警告ゼロ）

## 参考情報

- docs/implementation/tasks/TASK-006b.md
- docs/specifications/ui-spec.md
- REQ-UI-007, REQ-INFRA-001
- 依存: TASK-006a（GPUIパネルトグル + プリセット切替）
