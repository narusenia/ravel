# Ravel — 実装計画

## 概要

Ravelの実装を7つのマイルストーンに分割。各マイルストーンは独立して動作確認可能な単位。依存関係を考慮し、基盤から積み上げる。

## マイルストーン

| MS | タイトル | 概要 | 主要タスク数 |
|----|----------|------|-------------|
| MS1 | Foundation | コアエンジン基盤、GPUIシェル、プロジェクトファイル | 8 |
| MS2 | Media Pipeline | メディアI/O、オーディオ再生、基本タイムライン | 5 |
| MS3 | Node Editor | ノードグラフUI、基本ノード、ノード-タイムライン連携 | 5 |
| MS4 | Rendering | レンダリングパイプライン、キャッシュ、エクスポート | 4 |
| MS5 | Motion Graphics | モーグラ機能、タイポグラフィ、オーディオリアクティブ | 5 |
| MS6 | Pro Features | OCIO、OpenFX、Lua、カスタムシェーダ | 5 |
| MS7 | Polish | テーマ、プリセット、プラグインマネージャ、アップデート、i18n | 5 |

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
