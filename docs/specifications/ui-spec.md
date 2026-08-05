# Ravel — UI 仕様書

> 最終更新: 2026-08-03

この文書は**索引**。各ビューの仕様は `docs/specifications/ui/` に分かれている。

| ビュー | ファイル |
|---|---|
| ワークスペース・レイアウト・ウィンドウ | [ui/workspaces.md](ui/workspaces.md) |
| Outliner | [ui/outliner.md](ui/outliner.md) |
| Node Graph Editor | [ui/node-editor.md](ui/node-editor.md) |
| Timeline（ドープシート / カーブエディタ） | [ui/timeline.md](ui/timeline.md) |
| Viewer | [ui/viewer.md](ui/viewer.md) |
| Properties Inspector | [ui/properties.md](ui/properties.md) |
| Media Bin | [ui/media-bin.md](ui/media-bin.md) |
| テーマ | [ui/theme.md](ui/theme.md) |
| キーバインド | [ui/keybindings.md](ui/keybindings.md) |
| 設定ダイアログ（環境設定 / プロジェクト設定） | [ui/settings.md](ui/settings.md) |

**この文書は「こう動くべき」を書く。実装済みかどうかの正は
[`docs/ui-impl-status.md`](../ui-impl-status.md)。** 未実装の項目は各ビューの
仕様側にも「未実装。担当は `<計画>`」と明記する。

## 設計原則

- **ノードグラフファースト**: 絵を作る手段はノード DAG だけ。タイムラインや
  Viewer は DAG の外に別のデータ概念を持たない
- **Composition / Layer モデル（REQ-LAYER）**: タイムラインは After Effects 型の
  `Composition` が順序付き `Layer` を持つ形。各レイヤーは殻（時間配置・
  トランスフォーム・不透明度・ブレンドモード）であり、**1 つのノードネットワークを
  所有する**。殻の合成チェーンは合成ノードへコンパイルされ、レイヤーネットワークは
  ネットワーク境界ノードを通して再帰的に評価される
- **Timeline は 1 つのモデルの 2 つの見え方**: 同じ Composition を
  ドープシート（打点一覧）とカーブエディタ（グラフ）で切り替えて見る。
  トラック / クリップという別モデルは存在しない
- **選択の一元化**: レイヤー選択は `LayerSelection`、ノード選択は
  `CanvasSelection`、コンプは `ActiveComposition`、メディアは `MediaSelection`
  の各 Global が正。パネルは同じ Global を読み書きするので、パネル間の
  双方向同期プロトコルを持たない
- **コマンド経路の単一性**: キーバインド / メニュー / ボタンはすべて GPUI Action
  を経由し、`RavelWorkspace::dispatch_command()` が唯一の実行点
  （`.agents/rules/gpui.md`）
- **ワークスペース = N 個のウィンドウ**: 各ウィンドウが 1 本のレイアウトツリー
  （再帰 `Split` + タブ付き `Area`）を持ち、メインウィンドウは `windows[0]` に
  過ぎない。ドッキング UI は独自実装（`ravel-dock`）。同じパネルを何枚でも開け、
  ビュー状態はインスタンスごとに独立する（[ui/workspaces.md](ui/workspaces.md)）

## ポインタフィードバック

canvas 上のカーソルは既存の click / drag と同じヒットテストを使い、操作できる
対象だけを示す。ドラッグ中はポインタが元の対象を外れてもジェスチャーの
カーソルを維持する。

| パネル | hover | ドラッグ中 |
|---|---|---|
| Timeline | ルーラー / トリム端=`ResizeLeftRight`、バー=`OpenHand`、ロック=`OperationNotAllowed`、キー / グラフアンカー=`PointingHand`、グラフ接線 / 空白=`Crosshair` | スクラブ / トリム=`ResizeLeftRight`、移動=`ClosedHand`、並べ替え=`ResizeUpDown`、範囲 / 接線=`Crosshair` |
| Node Graph Editor | ポート / 空白=`Crosshair`、ノード=`OpenHand`、エッジ=`PointingHand` | 接続=`Crosshair`（スナップ時 `DragLink`）、ノード移動 / パン=`ClosedHand`、矩形選択=`Crosshair` |
| Viewer | 描画=`Crosshair`、選択本体=`OpenHand`、パスアンカー=`PointingHand`、接線=`Crosshair`、閉路可能な始点=`DragCopy` | パン / 本体 / アンカー=`ClosedHand`、描画 / 接線=`Crosshair` |
| Outliner | 行=`PointingHand` | レイヤー並べ替え=`ResizeUpDown` |

Viewer の Hand / Zoom ツールと bbox の 8 ハンドルには、対応する操作が未実装のため
カーソルを割り当てない。

## パネル一覧

状態は `✅` 実装済み / `🔲` 未実装（`PlaceholderPanel`）。

| パネル | 説明 | 状態 | 既定で配置されるプリセット |
|---|---|---|---|
| Outliner | Composition → Layer → Node の 3 階層プロジェクト構造ツリー | ✅ | Edit, Node, Motion |
| Node Graph Editor | ノードネットワークの編集。1 レイヤー 1 ネットワーク | ✅ | 全プリセット |
| Timeline | アクティブコンプの時間ビュー。ドープシート / カーブエディタ切替 | ✅ | Edit |
| Viewer | プレビュー表示とツール操作 | ✅ | 全プリセット |
| Properties Inspector | 選択対象（レイヤー / ノード / コンプ / メディア）の編集 | ✅ | Edit, Node, Motion |
| Media Bin | プロジェクトのメディアアセット管理 | ✅ | Edit |
| Dopesheet | 打点一覧の独立パネル | 🔲 | Node, Motion, Color |
| Scopes (Waveform / Vectorscope / Histogram / Parade) | 波形・ベクトルスコープ・ヒストグラム・パレード | 🔲 | Color |
| Text Editor | タイポグラフィ編集 | 🔲 | Motion |
| Render Queue | レンダージョブ管理 | 🔲 | （どのプリセットにも無い） |
| Shader Editor | WGSL カスタムシェーダ編集 | 🔲 | （どのプリセットにも無い） |
| Lua Console | スクリプトエディタ / コンソール | 🔲 | （どのプリセットにも無い） |

`🔲` のパネルもワークスペースプリセット（`assets/workspaces/*.toml`）は実際に
配置しており、開くとプレースホルダが出る。16 種すべてに View メニューの表示
トグルがあるので、プリセットが配置していないパネルも View メニューから出せる。
担当計画:
Dopesheet とカーブエディタの縦ズームは `PARAM-5`、スコープ 4 種は
`viewer-inspection-plan.md` の `INSP-5`（引き取り判断）、Text Editor は
`typography-plan.md`、Render Queue は `render-export-plan.md`、
Shader Editor と Lua Console は REQ-CODE-001。

## サブグラフ

| 種類 | 用途 | コンテキスト | 状態 |
|---|---|---|---|
| **Subnet** | ネットワーク内のノードを 1 ノードに畳む | 親と同じ解像度 / FPS / 尺で評価。In / Out ポートで外と接続 | ✅ 生成 UI は未実装（`NETIF-5`） |
| **Composition** | 独立コンポジション（AE のプリコンプ相当） | 独自の解像度 / FPS / 尺を持つ | ✅ |

- Subnet ノードはバッジ付きで表示し、ダブルクリックで中へ潜る（パンくず表示）
- **選択ノードを Subnet に畳む操作（`Ctrl+G` 相当）は未実装。**
  担当は `network-interface-editing-plan.md` の `NETIF-5`
- Composition はレイヤーとして他のコンポジションに置ける

## 制約・前提条件

- UI フレームワークは **gpui-ce**（`Cargo.toml` で rev 固定）と
  gpui-component。ネイティブメニューの挙動は OS 間で差がある
- ネイティブメニューのチェックマーク表示は `gpui::MenuItem::Action` に
  checked variant が無いため未対応。ヘッドレスモデル層（`ravel_ui::menu`）では
  正しく追跡している。カスタムメニュー描画での対応は将来
- スクリーンリーダー対応は GPUI のカスタムレンダリング特性上テキスト要素に限る
- ドッキングの残る制約（ビュー状態のウィンドウ間移送、ドラッグプレビュー、
  ビューア専用全画面ウィンドウ、分離ウィンドウへの OCIO 適用）は
  [ui/workspaces.md](ui/workspaces.md#制約--できないこと) にまとめてある
- ユーザー定義のキーバインド上書きはファイル（`<config>/ravel/keybindings.toml`）
  で可能。画面からの編集は未実装で、環境設定にあるのは読み取り専用の一覧まで
  （`SET-12`）
- 設定ダイアログの意図した挙動は [ui/settings.md](ui/settings.md)。どの項目が
  今出ていてどれが実際に効くかは [../ui-impl-status.md](../ui-impl-status.md) が
  正典。「出す項目 = 効く項目」が規約なので、前提機能が未実装の設定は画面に
  存在しない（`settings-screen-plan.md`）

関連要件: REQ-UI-001〜013、REQ-LAYER、REQ-PROJ-004。
