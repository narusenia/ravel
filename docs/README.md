# Ravel ドキュメント索引

Ravel は Rust 製のデスクトップアプリケーション（動画編集 + プロシージャル
モーショングラフィックス）。コアモデルは不変ノード DAG で、タイムラインは
After Effects 型の `Composition` / `Layer`。

リポジトリ全体の地図と規約は [`../AGENTS.md`](../AGENTS.md)。

## 役割で引く

| 知りたいこと | 場所 |
|---|---|
| **何かを追加する手順**（ノード / パネル / コマンド / ロケール） | [dev/](dev/) |
| **守るべき規約**（Rust / GPUI / 文書） | [`../.agents/rules/`](../.agents/rules/) |
| **型と関数の地図** | [agent-api-reference.md](agent-api-reference.md) |
| **GPUI のパターン集** | [gpui-ui-guide.md](gpui-ui-guide.md) |
| **どう振る舞うべきか**（設計意図） | [specifications/](specifications/) |
| **今どこまで動くか** | [ui-impl-status.md](ui-impl-status.md) |
| **何をどの順で作るか** | [implementation/](implementation/) |
| **何が壊れているか** | [`../issues/`](../issues/) |

同じ内容を 2 箇所に書かない。実装と食い違うときは**実装が正**で、
気づいた文書をその変更で直す。

## 要件

[requirements/](requirements/) — プロダクト要件。ID は `REQ-<領域>-<番号>`。

| ファイル | 領域 |
|---|---|
| [overview.md](requirements/overview.md) | 全体像 |
| [REQ-CORE.md](requirements/REQ-CORE.md) | 評価器・グラフ・データ型 |
| [REQ-LAYER.md](requirements/REQ-LAYER.md) | Composition / Layer・レイヤーネットワーク |
| [REQ-UI.md](requirements/REQ-UI.md) | パネル・ツール・テーマ・キーバインド |
| [REQ-MOGRAPH.md](requirements/REQ-MOGRAPH.md) | モーショングラフィックス |
| [REQ-MEDIA.md](requirements/REQ-MEDIA.md) | メディア入出力・音声 |
| [REQ-RENDER.md](requirements/REQ-RENDER.md) | レンダリング・書き出し |
| [REQ-GPU.md](requirements/REQ-GPU.md) | GPU |
| [REQ-3D.md](requirements/REQ-3D.md) | 3D |
| [REQ-DATA.md](requirements/REQ-DATA.md) / [REQ-PROJ.md](requirements/REQ-PROJ.md) | データ・プロジェクト |
| [REQ-CODE.md](requirements/REQ-CODE.md) / [REQ-PLUGIN.md](requirements/REQ-PLUGIN.md) | スクリプト・プラグイン |
| [REQ-INFRA.md](requirements/REQ-INFRA.md) | インフラ |

## 仕様

[specifications/](specifications/) — アーキテクチャとデータモデル、UI。

| ファイル | 内容 |
|---|---|
| [architecture.md](specifications/architecture.md) | クレート構成、評価パイプライン |
| [data-model.md](specifications/data-model.md) | Document / Composition / Layer / Graph |
| [procedural-geometry.md](specifications/procedural-geometry.md) | ジオメトリと属性の設計原則 |
| [ui-spec.md](specifications/ui-spec.md) | **UI 仕様の索引**（設計原則・パネル一覧・制約） |
| [ui/](specifications/ui/) | ビューごとの仕様（viewer / timeline / node-editor / outliner / properties / media-bin / theme / keybindings / workspaces） |

## 実装

[implementation/](implementation/) — 計画と順序。

| ファイル | 役割 |
|---|---|
| [backlog.md](implementation/backlog.md) | **全実装単位を 1 枚に並べた表**。着手できるものを探す入口 |
| [roadmap.md](implementation/roadmap.md) | **どの順でやるか、なぜその順か**。フェーズと並べる基準 |
| [README.md](implementation/README.md) | 計画書の索引（進行中 / 予定 / 完了） |
| [plan.md](implementation/plan.md) | サブシステム別の実装概要 |
| [perf-baseline.md](implementation/perf-baseline.md) | 評価とレンダーの測定値 |
| `*-plan.md` | 機能ごとの計画書（設計の正） |
| [done/](implementation/done/) | 完了した計画（雛形としても使う） |
| [archive/](implementation/archive/) | 旧 TASK-ID 世代。**由来の記録であって現行設計ではない** |

## 課題

[`../issues/`](../issues/) — 深刻度別（critical / high / medium / low）の台帳。
索引は [`../issues/README.md`](../issues/README.md)。**着手順は持たない**
（順序は roadmap が決める）。バグ・性能問題・技術的負債はここで、実装単位は
backlog で追う。
