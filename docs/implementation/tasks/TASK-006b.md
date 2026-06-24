# TASK-006b: GPUIパネルデタッチ/復帰 + Windows CI
- **マイルストーン**: MS1 Foundation
- **関連要件**: REQ-UI-007, REQ-INFRA-001
- **規模**: M
- **依存タスク**: TASK-006a

## 概要

GPUI実結線の第3弾。パネルを別ウィンドウにデタッチ/復帰する機能と、Windows CIビルド対応。

## 実装ステップ

1. **パネルデタッチ** — PanelDetach(Cmd+Shift+D)でフォーカス中のパネルをDockAreaから除去→新規ウィンドウで表示
2. **パネル復帰** — デタッチウィンドウclose時 or PanelReattach(Cmd+Shift+R)でメインウィンドウのDockAreaに復帰
3. **WindowManager連動** — AppShell.windows()のdetach/reattach状態と同期
4. **Windows CI** — gpui 0.2.2のWindows対応状況を確認。非対応の場合はcfgゲートでバイナリを条件付きビルド

## 完了条件

- [ ] Cmd+Shift+Dでフォーカスパネルが別ウィンドウに表示される
- [ ] デタッチウィンドウをcloseするとパネルがメインウィンドウに復帰
- [ ] Cmd+Shift+Rでキーボードから復帰可能
- [ ] WindowManagerの状態がdetach/reattachサイクルで整合
- [ ] Windows CIビルドがパス（またはcfgゲートで適切に分離）
- [ ] 既存テスト全パス
