# Ravel — 実装計画

## 概要

Ravelの実装を7つのマイルストーンに分割。各マイルストーンは独立して動作確認可能な単位。依存関係を考慮し、基盤から積み上げる。

## マイルストーン

| MS | タイトル | 概要 | タスク | 進捗 | ステータス |
|----|----------|------|--------|------|-----------|
| MS1 | Foundation | コアエンジン基盤、GPUIシェル、プロジェクトファイル | 10 | 10/10 | ✅ Done |
| MS2 | Media Pipeline | メディアI/O、オーディオ再生、基本タイムライン | 5 | 3/5 | In Progress |
| MS3 | Node Editor | ノードグラフUI、基本ノード、ノード-タイムライン連携 | 5 | 0/5 | Not Started |
| MS4 | Rendering | レンダリングパイプライン、キャッシュ、エクスポート | 4 | 0/4 | Not Started |
| MS5 | Motion Graphics | モーグラ機能、タイポグラフィ、オーディオリアクティブ | 5 | 0/5 | Not Started |
| MS6 | Pro Features | OCIO、OpenFX、Lua、カスタムシェーダ | 5 | 0/5 | Not Started |
| MS7 | Polish | テーマ、プリセット、プラグインマネージャ、アップデート、i18n | 5 | 0/5 | Not Started |

## マイルストーン依存関係

```
MS1 Foundation
 ├──→ MS2 Media Pipeline
 │     └──→ MS4 Rendering
 │           └──→ MS6 Pro Features
 ├──→ MS3 Node Editor
 │     ├──→ MS4 Rendering
 │     └──→ MS5 Motion Graphics
 │           └──→ MS6 Pro Features
 └──→ MS7 Polish (MS4完了後に開始)
```

## タスク進捗一覧

### MS1: Foundation

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-001 | 型システム + ノードグラフデータモデル | L | - | ✅ Done |
| TASK-002 | DAG評価エンジン | L | 001 | ✅ Done |
| TASK-003 | スレッディングモデル + CI基盤 | M | 001 | ✅ Done |
| TASK-004 | アンドゥシステム | M | 001 | ✅ Done |
| TASK-005 | wgpu GPU計算パイプライン基盤 | L | 003 | ✅ Done |
| TASK-006 | GPUIアプリケーションシェル | L | 005 | ✅ Done |
| TASK-006a | GPUIパネルトグル+プリセット切替 | M | 006 | ✅ Done |
| TASK-006b | GPUIデタッチ/復帰+Windows CI | M | 006a | ✅ Done |
| TASK-007 | アニメーションチャネルシステム | M | 001,002 | ✅ Done |
| TASK-008 | プロジェクトファイル (.ravprj) | M | 001,004 | ✅ Done |

> **TASK-006 完了（2026-06-24）**: フレームワーク非依存のヘッドレスロジック層（コマンドテーブル、メニュー定義、キーバインド機構、
> プリセット4種、パネル/レイアウトモデル、デタッチ状態管理）＋ GPUI実結線を全て実装完了。
> **TASK-006a**: パネルトグル＋プリセット切替のGPUI実結線。View/Workspaceメニュー連動、DockArea再構築、Outliner/Dopesheetパネル追加。
> **TASK-006b**: パネルデタッチ/復帰のGPUI実結線。DetachedWindows管理、on_release復帰、Windows CI確認。235テスト全pass。
> メニューチェックマークはモデル層で正常動作、GPUI描画側制約で未反映（将来カスタムメニューで対応）。
> デタッチウィンドウタイトルはi18n対応済み（PR #22）。
> **レイアウト保持修正（2026-06-25, PR #23）**: toggle/detach/reattach でレイアウトがリセットされる問題を修正。
> DockArea の add_panel/remove_panel による差分操作 + スナップショットベースの位置復元。
> プリセット切替時のみフル rebuild。PanelRegistry 登録で load() による復元に対応。
> 詳細は issue #10（GPUI実結線）, #11（workspace asset未使用）。

### MS2: Media Pipeline

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-009 | FFmpegデコード/エンコード統合 | L | 003 | ✅ Done |
| TASK-010 | ハードウェアデコーダ統合 | L | 009,005 | ✅ Done |
| TASK-011 | オーディオエンジン (CPAL+DSP) | M | 003 | ✅ Done |
| TASK-012 | タイムライン基盤 + メディアビン | L | 009,011,006 | 🔧 In Progress |
| TASK-013 | 映像/音声同期再生 | M | 012 | 🔲 Not Started |

### MS3: Node Editor

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-014 | ノードグラフエディタUI基盤 | L | 006 | 🔧 In Progress |
| TASK-015 | ノードグラフインタラクション | L | 014 | 🔲 Not Started |
| TASK-016 | ビルトインノード実装 (基本セット) | L | 002,005 | 🔲 Not Started |
| TASK-017 | タイムライン-ノードグラフ連携 | M | 015,012 | 🔲 Not Started |
| TASK-018 | カーブエディタ | M | 007,014 | 🔲 Not Started |

> **TASK-014 設計メモ（2026-06-26）**:
> - gpui-flow (`narusenia/gpui-flow` feat/gpui-ce-compat) を Ravel 専用に改造して使用。upstream は3ヶ月更新なし。
> - 統合方式: Cargo 依存 + アダプタ層。FlowState=UI層、Graph=評価層の二層構造。
> - ノード描画: カスタム node_renderer で縦ポートリスト描画。gpui-component (Label/Button等) をノード内で使用。
> - ポート色: DataTypeId → Hsla マッピングを ravel-ui に定義（FRAME_BUFFER=橙, SCALAR=灰, VEC=紫, COLOR=黄, TIME=青, AUDIO=緑, TEXT=白）。
> - find_handle_center を複数ハンドル対応に拡張（HandleDef.id でポートインデックス識別）。
> - サブグラフ/パンくずリスト: 今回スキップ。Graph にネスト構造がないため TASK-015 以降でグループ化と一緒に実装。
> - TASK-016 からノード登録システム + デモ用基本ノード（Constant, Merge, Blur, Transform, ColorCorrect）を前倒し。GPU シェーダは後回し（CPU fallback）。

### MS4: Rendering

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-019 | 三層キャッシュシステム | L | 005,002 | 🔲 Not Started |
| TASK-020 | レンダーキュー | M | 019,002 | 🔲 Not Started |
| TASK-021 | Write Node | S | 002 | 🔲 Not Started |
| TASK-022 | エクスポートパイプライン | M | 020,009 | 🔲 Not Started |

### MS5: Motion Graphics

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-023 | シェイプ生成 + リピーターノード | M | 016,007 | 🔲 Not Started |
| TASK-024 | パーティクル + フィールド/フォース | L | 023 | 🔲 Not Started |
| TASK-025 | プロシージャルタイポグラフィエンジン | L | 023 | 🔲 Not Started |
| TASK-026 | 3D基本機能 | L | 005,025 | 🔲 Not Started |
| TASK-027 | オーディオリアクティブシステム | M | 011,007 | 🔲 Not Started |

### MS6: Pro Features

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-028 | OpenColorIO統合 + GPU LUT | L | 005 | 🔲 Not Started |
| TASK-029 | OpenFXホスト基盤 + ネイティブプラグインAPI | L | 002,005 | 🔲 Not Started |
| TASK-030 | OFXプラグインプロセス分離実行 | L | 029 | 🔲 Not Started |
| TASK-031 | Luaスクリプティング環境 | M | 007 | 🔲 Not Started |
| TASK-032 | WGSLカスタムシェーダノード | M | 005,014 | 🔲 Not Started |

### MS7: Polish

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-033 | テーマシステム + アクセシビリティ | M | 006 | 🔲 Not Started |
| TASK-034 | プリセット/テンプレートシステム | M | 015,008 | 🔲 Not Started |
| TASK-035 | プラグインマネージャUI | S | 029,031,032 | 🔲 Not Started |
| TASK-036 | 自動アップデーター | M | 006 | 🔲 Not Started |
| TASK-037 | i18n基盤 + ドキュメント | M | 006 | 🔧 In Progress (i18n完了, docs残) |

## 実装順序

### MS1: Foundation

```
TASK-001 → TASK-002 → TASK-003 → TASK-004 (直列、基盤依存)
                                    ↓
TASK-005 (並列可)  TASK-006 (並列可)  TASK-007 (並列可)
                                    ↓
                              TASK-008 (統合)
```

### MS2: Media Pipeline

```
TASK-009 → TASK-010 (FFmpeg → HWデコード)
TASK-011 (並列可、オーディオ)
TASK-009 + TASK-011 → TASK-012 (タイムライン基盤)
TASK-012 → TASK-013 (同期再生)
```

### MS3: Node Editor

```
TASK-014 → TASK-015 (UIフレーム → ノード操作)
TASK-016 (並列可、基本ノード実装)
TASK-015 + TASK-016 → TASK-017 (タイムライン連携)
TASK-017 → TASK-018 (カーブエディタ)
```

### MS4: Rendering

```
TASK-019 → TASK-020 (三層キャッシュ → レンダーキュー)
TASK-021 (並列可、Write Node)
TASK-020 → TASK-022 (エクスポートパイプライン)
```

### MS5: Motion Graphics

```
TASK-023 → TASK-024 (シェイプ → パーティクル/フィールド)
TASK-025 (並列可、タイポグラフィ)
TASK-026 (並列可、3D基本)
TASK-023〜026 → TASK-027 (オーディオリアクティブ)
```

### MS6: Pro Features

```
TASK-028 (OCIO、独立)
TASK-029 → TASK-030 (OFXホスト → プラグイン実行)
TASK-031 (Lua、独立)
TASK-032 (WGSLシェーダノード、独立)
```

### MS7: Polish

```
TASK-033 (テーマ/a11y)
TASK-034 (プリセット/テンプレート)
TASK-035 (プラグインマネージャ)
TASK-036 (アップデーター)
TASK-037 (i18n/ドキュメント)
全て並列可
```

## トレーサビリティマトリクス

| REQ-ID | タスク |
|--------|--------|
| REQ-CORE-001 | TASK-001, TASK-002 |
| REQ-CORE-002 | TASK-002 |
| REQ-CORE-003 | TASK-001 |
| REQ-CORE-004 | TASK-004 |
| REQ-CORE-005 | TASK-003 |
| REQ-CORE-006 | TASK-019 |
| REQ-CORE-007 | TASK-007, TASK-018 |
| REQ-CORE-008 | TASK-012 |
| REQ-CORE-009 | TASK-001 |
| REQ-GPU-001 | TASK-005 |
| REQ-GPU-002 | TASK-005 |
| REQ-GPU-003 | TASK-032 |
| REQ-UI-001 | TASK-006 |
| REQ-UI-002 | TASK-014, TASK-015 |
| REQ-UI-003 | TASK-012, TASK-017 |
| REQ-UI-004 | TASK-006, TASK-028 |
| REQ-UI-005 | TASK-006 |
| REQ-UI-006 | TASK-033 |
| REQ-UI-007 | TASK-006 |
| REQ-UI-008 | TASK-012 |
| REQ-UI-009 | TASK-006 |
| REQ-UI-010 | TASK-015 |
| REQ-MEDIA-001 | TASK-009, TASK-010 |
| REQ-MEDIA-002 | TASK-011 |
| REQ-MEDIA-003 | TASK-027 |
| REQ-MOGRAPH-001 | TASK-023 |
| REQ-MOGRAPH-002 | TASK-024 |
| REQ-MOGRAPH-003 | TASK-026 |
| REQ-MOGRAPH-004 | TASK-025 |
| REQ-MOGRAPH-005 | TASK-016, TASK-023 |
| REQ-RENDER-001 | TASK-020 |
| REQ-RENDER-002 | TASK-021 |
| REQ-RENDER-003 | TASK-028 |
| REQ-PLUGIN-001 | TASK-029, TASK-030 |
| REQ-PLUGIN-002 | TASK-029 |
| REQ-PLUGIN-003 | TASK-031 |
| REQ-PLUGIN-004 | TASK-035 |
| REQ-PLUGIN-005 | TASK-034 |
| REQ-PROJ-001 | TASK-008 |
| REQ-PROJ-002 | TASK-004, TASK-030 |
| REQ-PROJ-003 | TASK-008 |
| REQ-PROJ-004 | TASK-008 |
| REQ-PROJ-005 | TASK-008 |
| REQ-INFRA-001 | TASK-003, TASK-005, TASK-006 |
| REQ-INFRA-002 | TASK-037 |
| REQ-INFRA-003 | TASK-036 |
| REQ-INFRA-004 | TASK-003 (CI基盤) |
| REQ-INFRA-005 | TASK-037 |
| REQ-INFRA-006 | TASK-003 |
| REQ-INFRA-007 | TASK-030, TASK-031 |
| REQ-INFRA-008 | TASK-001 (ライセンスヘッダ) |
