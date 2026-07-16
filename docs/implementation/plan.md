# Ravel — 実装計画

## 概要

Ravelの実装を7つのマイルストーンに分割。各マイルストーンは独立して動作確認可能な単位。依存関係を考慮し、基盤から積み上げる。

## マイルストーン

| MS | タイトル | 概要 | タスク | 進捗 | ステータス |
|----|----------|------|--------|------|-----------|
| MS1 | Foundation | コアエンジン基盤、GPUIシェル、プロジェクトファイル | 10 | 10/10 | ✅ Done |
| MS2 | Media Pipeline | メディアI/O、オーディオ再生、基本タイムライン | 5 | 3/5 | In Progress |
| MS3 | Node Editor + Composition | ノードグラフUI、基本ノード、Composition/Layerモデル | 6 | 4/6 | In Progress |
| MS4 | Rendering | レンダリングパイプライン、キャッシュ、エクスポート | 4 | 0/4 | Not Started |
| MS5 | Motion Graphics | 属性/フィールド/sim基盤、モーグラ機能、タイポグラフィ、オーディオリアクティブ | 15 | 0/15 | Not Started |
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
| TASK-014 | ノードグラフエディタUI基盤 | L | 006 | ✅ Done |
| TASK-015 | ノードグラフインタラクション | L | 014 | ✅ Done |
| TASK-016 | ビルトインノード実装 (基本セット) | L | 002,005 | ✅ Done |
| TASK-017 | ノードエディタ Interaction 強化 + Properties パネル | L | 015,016 | ✅ Done |
| TASK-018 | カーブエディタ | M | 007,014 | 🔲 Not Started |
| TASK-019 | Composition/Layer モデル移行（AEモデル） | XL | 016,017 | 🔲 Not Started |

> **TASK-014 完了（2026-06-26）**:
> gpui-flow 依存を削除し、canvas ベースの直接実装に移行完了。
> - `ravel-app/src/node_editor/viewport.rs`: Viewport 座標変換（flow↔screen、zoom_toward、fit_to_content）
> - `ravel-app/src/node_editor/bezier.rs`: ベジェ曲線計算（horizontal_bezier、ヒットテスト距離計算）
> - `ravel-app/src/node_editor/painting.rs`: canvas 描画関数群（グリッド、ノード、エッジ、ポート、ドラフト線、ポートヒットテスト、スナップ検出）
> - `ravel-app/src/node_editor/port_colors.rs`: DataTypeId → Hsla（前回セッションから維持）
> - `ravel-app/src/panels/node_editor.rs`: パネル全面リライト（DragMode: Pan/MoveNodes/Connect、scroll_wheel ズーム、ポートドラッグ→エッジ作成）
> - `ravel-core/src/registry/`: NodeRegistry + builtin 5種（前回セッションから維持）
> - 削除: adapter.rs（FlowNode/FlowEdge 変換）、node_renderer.rs（gpui-flow FlowNode 描画）
>
> **TASK-015 完了（2026-06-26）**:
> UndoStack 統合、エッジ選択/ノード・エッジ削除、矩形選択、コンテキストメニュー（ノード追加）、
> pinch ズーム、グリッドスナップ（20px）を実装。ミニマップは後続タスクに送り。
>
> **TASK-016 完了（2026-06-29）**:
> `ravel-nodes` クレート新設。5つのビルトインプロセッサ（Constant/ColorCorrect/Blur/Transform/Merge）
> + WGSLシェーダ4本 + `register_all_processors()` 関数。16テスト。PR #31。
>
> **TASK-017 完了（2026-06-29）**:
> Phase 0: Evaluator/GpuContext/ShaderManager を NodeEditorPanel に統合（PR #32）。
> Phase 1: Properties パネル実装 — PropertySection/PropertyField データモデル、Accordion レイアウト、
> Slider/Select ウィジェット、SelectedPropertiesTarget/PropertyChanged Global シグナル、
> undo/redo 連携（PR #33）。
> Phase 2-6: コンテキストメニュー充実（Delete Node/Bypass）、エッジスタイル切替（Bezier/Straight/Step）、
> Copy/Paste/Duplicate（Cmd+C/V/D）、ポート型フィルタリング、単一入力制約、Fit View（F key）（PR #34）。
> 将来: Phase 7（パラメータ InputPort 化）、Phase 8（アセット・アイコン）。

### MS4: Rendering

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-020 | 三層キャッシュシステム | L | 005,002 | 🔲 Not Started |
| TASK-021 | レンダーキュー | M | 020,002 | 🔲 Not Started |
| TASK-022 | Write Node | S | 002 | 🔲 Not Started |
| TASK-023 | エクスポートパイプライン | M | 021,009 | 🔲 Not Started |

### MS5: Motion Graphics

プロシージャルジオメトリ基盤（TASK-038〜041、`docs/specifications/procedural-geometry.md`）を
先行させ、その上に v2 要件のモーグラ機能を実装する。

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-038 | ジオメトリコンテナ + 属性システム | L | — | 🔲 Not Started |
| TASK-039 | 属性操作ノード群 | M | 038 | 🔲 Not Started |
| TASK-040 | 汎用フィールド型 + ビルトインフィールド | L | 038 | 🔲 Not Started |
| TASK-041 | ステートフル評価 + simキャッシュ | L | 038 | 🔲 Not Started |
| TASK-042 | ジオメトリラスタライズノード | M | 038 | 🔲 Not Started |
| TASK-043 | シェイプのジオメトリ化 + インスタンス複製 | L | 038,042 | 🔲 Not Started |
| TASK-044 | per-instance 変調（falloff 相当） | M | 040,043 | 🔲 Not Started |
| TASK-045 | パーティクル v2（ポイントジオメトリ sim） | L | 040,041,042 | 🔲 Not Started |
| TASK-046 | テーブルデータ入力 + 属性バインディング | M | 038,043 | 🔲 Not Started |
| TASK-047 | 属性スプレッドシートパネル | M | 038 | 🔲 Not Started |
| TASK-024 | シェイプ生成 + リピーターノード | M | 016,007 | 🔁 Superseded → TASK-043 |
| TASK-025 | パーティクル + フィールド/フォース | L | 024 | 🔁 Superseded → TASK-045 |
| TASK-026 | プロシージャルタイポグラフィエンジン | L | 024 | 🔲 Not Started |
| TASK-027 | 3D基本機能 | L | 005,026 | 🔲 Not Started |
| TASK-028 | オーディオリアクティブシステム | M | 011,007 | 🔲 Not Started |

### MS6: Pro Features

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-029 | OpenColorIO統合 + GPU LUT | L | 005 | 🔲 Not Started |
| TASK-030 | OpenFXホスト基盤 + ネイティブプラグインAPI | L | 002,005 | 🔲 Not Started |
| TASK-031 | OFXプラグインプロセス分離実行 | L | 030 | 🔲 Not Started |
| TASK-032 | Luaスクリプティング環境 | M | 007 | 🔲 Not Started |
| TASK-033 | WGSLカスタムシェーダノード | M | 005,014 | 🔲 Not Started |

### MS7: Polish

| タスク | タイトル | 規模 | 依存 | ステータス |
|--------|---------|------|------|-----------|
| TASK-034 | テーマシステム + アクセシビリティ | M | 006 | 🔲 Not Started |
| TASK-035 | プリセット/テンプレートシステム | M | 015,008 | 🔲 Not Started |
| TASK-036 | プラグインマネージャUI | S | 030,032,033 | 🔲 Not Started |
| TASK-037 | 自動アップデーター | M | 006 | 🔲 Not Started |
| TASK-038 | i18n基盤 + ドキュメント | M | 006 | 🔧 In Progress (i18n完了, docs残) |

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
| REQ-CORE-010 | TASK-038, TASK-039, TASK-047 |
| REQ-CORE-011 | TASK-041 |
| REQ-CORE-012 | TASK-040, TASK-044 |
| REQ-CORE-013 | （v1 非採用 — MOGRAPH v2 実装後に再評価） |
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
| REQ-MOGRAPH-001 | TASK-023, TASK-042, TASK-043, TASK-044 |
| REQ-MOGRAPH-002 | TASK-045 |
| REQ-MOGRAPH-003 | TASK-026 |
| REQ-MOGRAPH-004 | TASK-025 |
| REQ-MOGRAPH-005 | TASK-016, TASK-023 |
| REQ-DATA-001 | TASK-046 |
| REQ-DATA-002 | TASK-046 |
| REQ-DATA-003 | （Could — 未計画） |
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
