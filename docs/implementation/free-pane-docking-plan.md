# フリードッキング実装計画（独自 Pane 配置システム）

> **Status**: Planned — 2026-07-31

対象要件: REQ-UI-005 (v2)、REQ-UI-009 (v2)。関連: REQ-UI-004、REQ-UI-013。

`panel-placement-plan.md`（#181）を **supersede** する。同計画の課題意識
（View トグルがプリセット配置依存）は本計画の DOCK-2 が既定スロット挿入として
引き継ぐ。

## 背景と問題

現在のドッキングは gpui-component の `DockArea` に依存し、ravel-app の
8 ファイル・約 590 行がベタ書きで触っている（`workspace.rs` の
`filter_panel_state` が `PanelState` / `PanelInfo::Stack` の内部構造まで
直接操作する最深部）。この構成には構造的な限界がある。

1. **パネルはシングルトン**。`panel_views: HashMap<PanelKind, _>` /
   `detached_panels: HashSet<PanelKind>` が「1 種 1 枚」を前提にしており、
   同一パネルの複数表示（Viewer 2 枚で別コンポ比較等）が表現できない。
2. **detach/reattach が「シングルトンの移動」として実装されている**ため、
   周辺に欠陥が集中している:
   - OS クローズボタンで shell が desync し、パネルが行方不明になる
     （MED-APP-01。`reattach_window` は呼び出し元ゼロの dead API）
   - detach/reattach のたびに `refresh_panel_views()` が全パネルを作り直し、
     **全パネルのビュー状態がリセット**される
   - `pre_detach_snapshot` が最初の detach 時点で固定されるため、detach 中の
     ドック再配置が reattach で巻き戻る
   - 分離ウィンドウ（`DetachedPanelView`）はダイアログ・通知レイヤーを
     描画しないため、分離窓内のダイアログが不可視になる
   - `WindowPlacement`（配置の記録・復元）は未配線の dead API（LOW-APP-14）
3. **メイン窓と分離窓で実装が二重**。TitleBar もメイン = gpui-component
   カスタム / 分離 = OS ネイティブで不揃い。「分離窓だけ処理が抜けている」
   型のバグ（MED-APP-01、ダイアログ不可視）はこの二重実装の帰結。
4. パネル生成の分岐が `panel_for_kind`（レイアウト構築）と `register_panels`
   （`DockArea::load()` 復元）の 2 箇所に重複しており、同期し忘れると
   reattach で PlaceholderPanel に戻る。

一方、レイアウトの headless モデル（`LayoutNode` の Leaf/Split 二分木、
`WindowManager`、プリセット）は ravel-ui に既にあり、gpui-component の
シリアライズには依存していない。**移行の土台は揃っている**。

## 目的

- gpui-component の dock 依存を廃止し、独自のドッキング UI（新クレート
  `ravel-dock`）に置き換える（テーマ・汎用部品は引き続き gpui-component）
- 同一パネルの多重インスタンス表示（全 16 種、例外なし）
- タブ D&D + エリアメニューによる自由な分割・再配置
- 全ウィンドウ同型化により detach/reattach の欠陥群を構造的に排除
- 全窓共通のカスタム TitleBar と分離窓の AlwaysOnTop
- レイアウトの永続化（アプリレベル + `.ravprj` オプトイン埋込）

## 決定事項

対話設計（2026-07-31）で確定した事項。それぞれ却下した代替案を添える。

| 決定 | 採用 | 却下した代替案 |
|---|---|---|
| 配置モデル | 再帰 Split + エリア内タブ + D&D 再配置（フル docking） | Blender 純正（タブなし）はスコープ系 16 パネルで画面が破綻する |
| 多重インスタンスの状態 | Document/View 分離。モデル（プロジェクト・選択・アクティブコンプ・再生ヘッド）は共有、ビュー状態（ズーム/パン/表示対象）はインスタンス別 | 完全ミラーは複数表示の動機（別コンポ比較）を殺す。完全独立は Properties の追従先が曖昧になる |
| フローティング | detached OS ウィンドウに一本化 | 窓内オーバーレイパネルは z-order・ヒットテスト・ダイアログ層との競合管理に対して効果が薄い |
| ウィンドウモデル | 全窓同型 — ワークスペース = N 窓、各窓が 1 本のレイアウトツリー。メイン = windows[0] | 「1 窓 1 パネル」維持は二重実装（MED-APP-01 型バグの温床）を残す |
| 分離窓クローズ | インスタンス破棄（多重化により喪失なし）。戻しはタブの窓間 D&D と `Cmd+Shift+R` | 自動 reattach は「閉じたのにメインが増える」驚きを生む |
| メイン窓連動 | クローズ追従 + 最小化追従。フォーカスは独立 | フォーカス連動は AlwaysOnTop と噛み合わない |
| AlwaysOnTop | gpui-ce-ravel フォークに `set_always_on_top(bool)` を追加し実行時トグル | 生成時固定（`WindowKind::Floating`）は切替のたびに detach し直し。窓再生成はフリッカーと状態移植が必要 |
| TitleBar | 全窓共通コンポーネント、ベースは `gpui_component::TitleBar`（プラットフォーム差吸収済み） | 完全自作はトラフィックライト回避・Windows ボタン描画の再実装 |
| クレート構成 | モデル = ravel-ui（headless）、描画 = 新クレート ravel-dock、配線 = ravel-app | ravel-app 内モジュールは workspace.rs（1668 行）の肥大に積み増す。全部新クレートは既存 LayoutNode/プリセットと分断する |
| 永続化 | アプリレベル設定 + `.ravprj` オプトイン埋込（適用はセッション限定、アプリ既定不侵） | プロジェクト常時埋込は他人のプロジェクトで作業環境が上書きされる |
| 多重化範囲 | 全 16 パネル、例外なし | 制限リストはモデルと UI に例外分岐を漏らす |
| 移行 | ravel-dock を並行構築 → パリティ達成で一発カットオーバー | 実行時フラグ並存は workspace.rs の配線が二重メンテになる |
| 分割操作 | タブ D&D ドロップゾーン + エリアメニュー（複製分割含む） | コーナードラッグはタブバーとヒット競合し誤発動が多い |
| 現行バグ | 全て本計画で解消（現行系への先行修正はしない） | — |

補足決定:

- **プリセット切替はメイン窓のツリーだけを差し替える**。分離窓は触らない
  （プリセットは「メイン画面の作業モード」であり、分離窓はユーザーが
  意図して切り出した参照面）。
- **`Cmd+Shift+D`（detach）はフォーカス中のパネルインスタンスを新しい
  分離窓へ移す**。タブの窓外ドラッグと同じ操作の键盘版。
- Viewer 多重時の評価コストは可視インスタンス数に比例する。同一コンポ・
  同一フレームは既存の評価キャッシュを共有する。パネル → 評価要求の対応が
  1:1 から N:1 になる点は DOCK-2 のビュー状態設計で吸収する。

## 目標アーキテクチャ

```text
ravel-ui（headless。gpui はコード上未使用 — DOCK-1 で Cargo.toml の
未使用 gpui / gpui-component 依存も落とし、非依存を宣言どおりにする）
  WorkspaceLayout { windows: Vec<WindowLayout> }
  WindowLayout { id: WindowId, root: LayoutNode,
                 placement: WindowPlacement, always_on_top }
  （WindowId は論理 ID。ホストが GPUI の WindowHandle との対応表を持ち、
   windows[0] = メイン窓という規約で識別する）
  LayoutNode = Split { orientation, ratio, first, second }
             | Area { tabs: Vec<PanelInstance>, active }
  PanelInstance { id: PanelInstanceId, kind: PanelKind }
  操作: split / close_area / move_tab / detach_to_window / close_window /
        duplicate_instance / toggle_panel（既定スロット挿入）
        ─ すべて純関数的でユニットテスト可能
                │
                ▼
ravel-dock（新クレート: gpui + gpui-component テーマのみ、アプリロジック禁止）
  DockRoot ─ ツリーを描画。スプリッタ、タブバー、ドロップゾーン、
             エリアメニュー。操作は Layout 操作イベントとして emit
  PaneContent trait ─ パネル中身の供給界面（ravel-app が実装）
  examples/gallery ─ 単体検証用サンプルバイナリ
                │
                ▼
ravel-app（配線）
  1 つのパネルファクトリ（PanelKind → ビュー生成、二重登録の解消）
  ウィンドウホスト（全窓同型: TitleBar + DockRoot + ダイアログ/通知層）
  窓ライフサイクル連動、AlwaysOnTop、永続化 I/O
```

## 実装単位

### DOCK-1: レイアウトモデル v2（ravel-ui）

- `LayoutNode` を Split / Area（タブ列）の 2 種に再構成し、
  `PanelInstanceId` を導入する。既存プリセット（Leaf/Split）は Area 1 タブへ
  機械的に写像できるので、`BuiltinPreset` とアセット TOML を v2 形式へ更新する。
- `WorkspaceLayout`（N 窓）と `WindowLayout` を定義し、既存 `WindowManager` /
  `WindowPlacement` をこの下へ統合する。
- 操作関数: `split` / `close_area`（Split の畳み込み込み）/ `move_tab` /
  `detach_to_window` / `close_window` / `duplicate_instance`。
- serde（TOML/JSON）ラウンドトリップ。

**完了条件**

- 操作関数の網羅的ユニットテスト（空ツリー・単一 Area・深いネスト・
  最後のタブを移動したときの Area 消滅・最後の Area が消えた窓の扱い）。
- 旧形式プリセット → v2 の写像テスト。`asset_files_match_builtin_presets`
  相当のドリフト検出を v2 で維持。

### DOCK-2: シェル統合と既定スロット挿入（ravel-ui）

- `ShellState` がアクティブな `WorkspaceLayout` を保持する（#181 の
  「実効レイアウトの分離」に相当）。フォーカスは `PanelInstanceId` 単位 —
  `FocusedPanelGlobal` の中身も `Option<PanelKind>` から
  `Option<PanelInstanceId>` へ移行する。
- `PanelKind::default_slot() -> DockSlot`（Left/Right/Bottom/Center）を定義し、
  View トグルでツリーに無いパネルは既定スロットへ挿入、既にあるパネルは
  最初のインスタンスへフォーカス（または非表示化ではなく Area から除去）。
  **これが #181 の解消**。
- `CommandOutcome::{DetachPanel, ReattachPanel}` を新意味論
  （インスタンス detach / フォーカス窓の全パネルをメインへ戻す）で再定義。
- インスタンス別ビュー状態の置き場（`PanelInstanceId` キーのステート）を
  headless 側に定義する。

**完了条件**

- 各プリセットで 16 パネル全てをトグル → ツリーに Area が現れる
  テーブル駆動テスト（**#181 の回帰テスト**）。
- detach → クローズ → 再トグルでパネルが既定スロットに出る往復テスト。
- コマンド経路の headless テスト（`shell.rs` の既存テスト形式を踏襲）。

### DOCK-3: ravel-dock クレート骨格（静的描画）

- 新クレート `crates/ravel-dock`。依存は gpui + gpui-component（テーマ・
  基本部品のみ）。アプリロジック（PanelKind の知識）を持ち込まない —
  中身は `PaneContent` trait で外から供給する。
- `LayoutNode` ツリーの描画: スプリッタ（境界ドラッグでリサイズ、
  ratio を書き戻す）、タブバー（タブ切替、アクティブ表示）、
  エリアの空状態。
- `examples/gallery`: ダミーパネルでレイアウトを組む検証用バイナリ。
  カットオーバー前の実機確認はここで行う。

**完了条件**

- gallery で 4 プリセット相当のレイアウトが組めて、境界ドラッグで
  リサイズできる。
- テーマ（ダーク/ライト）が gpui-component の ActiveTheme に追従する。
- `cargo test -p ravel-dock`（レイアウト計算の px 変換等）が通る。

### DOCK-4: ravel-dock 対話（D&D・エリアメニュー）

- タブのドラッグ: エリア端 1/4 領域へのドロップ = その方向に分割、
  中央 = タブ合流、ドロップゾーンのハイライト表示。
- ウィンドウ外へのドロップ = detach 要求イベントを emit
  （窓生成はホスト責務）。
- 窓間移動（窓 A のタブを窓 B のドックへ）: GPUI にネイティブの
  クロスウィンドウドラッグは無いが、全窓が自前なので、ドラッグ中の
  グローバルカーソル位置と各窓の bounds のヒットテストで
  ドロップ先の窓・エリアを解決できる（DOCK-6 の対応表を使う）。
  窓をまたぐドラッグプレビュー描画だけが非対象（下記）。
- エリアメニュー（︙）: 右に分割 / 下に分割 / 複製して分割
  （同 PanelKind の新インスタンス）/ エリアを閉じる。
- ドラッグ状態の防御は既存規約に従う（`pressed_button` 確認、Esc キャンセル
  — MED-APP-03 と同型の穴を作らない）。

**完了条件**

- gallery で D&D 分割・合流・複製分割・閉じるが機能する。
- ドロップゾーン計算のユニットテスト（境界値）。
- ドラッグ中のカーソルフィードバックが
  `done/pointer-feedback-plan.md` の規約に従う。

### DOCK-5: gpui-ce-ravel フォークパッチ

- gpui 依存を `gpui-ce/gpui-ce` から `narusenia/gpui-ce-ravel` へ切り替える。
  対象は `gpui` と `gpui_platform` の両方の git 参照、および
  `[patch.crates-io]` / `[patch."https://github.com/zed-industries/zed"]` の
  両 patch 節。gpui-component（narusenia フォーク）が参照する gpui も
  同じツリーに解決されること（`cargo tree -i gpui` が 1 本）を確認する。
- `set_always_on_top(bool)` を追加（macOS = `setLevel_(NSFloatingWindowLevel /
  NSNormalWindowLevel)`。Windows / Linux は各プラットフォームの相当 API。
  未対応プラットフォームは no-op + 警告ログ）。
- メイン窓の最小化/復帰をホストが観測できる通知（`windowDidMiniaturize` /
  `windowDidDeminiaturize` 相当）が無ければ追加。
- 変更は upstream（gpui-ce）へ PR できる汎用 API の形に保つ。

**完了条件**

- Ravel が gpui-ce-ravel 参照でビルド・起動できる（挙動不変）。
- gallery または最小サンプルで AlwaysOnTop の実行時トグルが機能する。

### DOCK-6: マルチウィンドウホスト（ravel-app）

- 全窓同型のウィンドウホスト: TitleBar + `DockRoot` + ダイアログ層 +
  通知層（`Root::render_dialog_layer` / `render_notification_layer` を
  全窓に配置 — 分離窓ダイアログ不可視の解消）。
- セッション状態（プロジェクト・再生・音声・評価）は現在
  `RavelWorkspace::new` がメイン窓に抱えている。これを窓から独立した
  共有セッション（既存の Global 群）の所有に整理し、各窓ホストは
  「セッションを表示するビュー」に徹する。論理 `WindowId` ↔ GPUI
  `WindowHandle` の対応表もここが持つ（`DetachedWindowHandles` の後継）。
- 窓クローズ = レイアウトからの窓削除 + インスタンス破棄。
  `on_window_should_close` を全分離窓に登録（**MED-APP-01 の構造的解消**）。
- メイン窓連動: クローズ追従（メインが閉じたら全分離窓を閉じる）、
  最小化追従（DOCK-5 の通知で分離窓を隠す/戻す）。
- detach 要求（D&D / `Cmd+Shift+D`）で新窓を開き、対象インスタンスを移す。
  ビューエンティティは作り直さず移送する（**状態リセットの解消**）。

**完了条件**

- 分離窓のクローズボタン → シェル状態が一貫し、stale ハンドルが残らない
  統合テスト（`command_dispatch_repro.rs` の形式）。
- 分離窓でダイアログが表示される実機確認。
- メイン最小化 → 分離窓が隠れ、復帰で戻る実機確認。

### DOCK-7: TitleBar 共通化と AlwaysOnTop ピン

- `title_bar.rs` を全窓共通コンポーネント化: 共通部（ドラッグ領域 +
  ウィンドウコントロール）+ 窓種別スロット。メイン = プロジェクト名 +
  プリセット切替（現状維持）、分離 = 窓タイトル（アクティブパネル名、
  複数タブ時は「N panels」形式のロケールキー）+ AlwaysOnTop ピン。
- 分離窓の生成オプションを `TitleBar::title_bar_options()` に統一
  （OS ネイティブ装飾との不揃いを解消）。
- ピンの状態は `WindowLayout.always_on_top` に保持し、DOCK-5 の API で反映。

**完了条件**

- メイン/分離の TitleBar が同一コンポーネント経由で描画される。
- ピントグルが即時反映され、状態が窓ごとに独立している実機確認。
- タイトル文言がロケールキー経由（en/ja 両方に追加）。

### DOCK-8: カットオーバー

- `RavelWorkspace` の `DockArea` / `DockItem` / `PanelState` 配線を
  `ravel-dock` + DOCK-6 のホストに差し替え、`register_panels` /
  `panel_for_kind` の二重登録を単一ファクトリ
  （`PanelInstanceId` を受けてビューを生成・登録する 1 関数）に統合する。
- `panel_views: HashMap<PanelKind, _>` はインスタンス ID キーのレジストリに
  置き換える。`TimelinePanelHandle` / `NodeEditorHandle` のような
  シングルトン前提の Global ハンドルは「フォーカス中インスタンスへの
  ハンドル」として再定義する（フォーカス変更で差し替え）。
- キーバインド: `Cmd+F1`〜`F4`（プリセット）、`Cmd+Shift+D` / `Cmd+Shift+R`
  （新意味論）。View トグルは DOCK-2 の経路に接続。
- gpui-component dock 関連コード（`filter_panel_state` 等 約 590 行）と
  `eprintln!` ログ（LOW-APP-17 該当箇所）を削除。

**パリティ基準**（これを満たすまで旧系を消さない）

- 4 プリセットが現行と同等のレイアウトで表示される
- プリセット切替・パネルトグル・detach/reattach がキーバインドで機能する
- 既存の `command_dispatch_repro.rs` 統合テストが（意味論変更分の更新を
  除き）通る
- `gpui_component::dock` への参照がワークスペースから消える

**完了条件**

- パリティ基準の全項目 + `mise run check`。
- 16 パネル × 4 プリセットのトグル実機確認（#181 のクローズ根拠）。

### DOCK-9: 永続化とカスタムワークスペース

- アプリレベル: 全窓レイアウト + `WindowPlacement` + AlwaysOnTop を
  設定ディレクトリの専用 TOML（例: `<config>/ravel/layout.toml`）に保存、
  起動時復元（**LOW-APP-14 の解消**）。`settings.toml` の 4 層マージとは
  独立させ、`settings-screen-plan.md` のスコープと衝突させない。
- カスタムワークスペースの名前付き保存/復元 UI（`PresetLibrary::save_custom`
  への導線 — REQ-UI-005 の未達受入条件）。
- `.ravprj` 埋込: 保存ダイアログのオプトイントグル（既定 OFF）。開いたとき
  埋込があればセッションレイアウトとして適用するが、アプリレベルの既定は
  書き換えない。埋込なしプロジェクトはアプリレベルの前回レイアウトを使う。
- ワイヤ形式: DOCK-1 の serde 表現をそのまま使い、`layout.toml` /
  `.ravprj` 内エントリ（`workspace_layout.toml`、`ui_state.json` とは別
  エントリ）ともに `layout_version` フィールドを持たせる。現行アプリは
  レイアウトを一切永続化していないので後方移行は不要 — 未知バージョンと
  破損は「読めなければ既定レイアウト」に倒す。カスタムプリセットも
  同じ形式で `layout.toml` 側に保存する。非同期セーブのスナップショットに
  レイアウトを含める配線（`project_state` の保存経路）もここで足す。

**完了条件**

- 再起動でレイアウト・窓配置・AlwaysOnTop が復元されるテスト + 実機確認。
- 埋込あり/なしプロジェクトを交互に開いてもアプリ既定が汚れないテスト。
- 破損した layout.toml で既定レイアウトにフォールバックする（起動を
  妨げない）テスト。

### DOCK-10: 実機確認と文書更新

- 全プリセット × トグル × 分割 × detach × 永続化の実機確認（cliclick）。
- 文書: `docs/specifications/ui/workspaces.md` 全面改訂（v2 形式・操作・
  制約表）、`docs/specifications/ui-spec.md` のドッキング記述、
  `docs/ui-impl-status.md`、`docs/dev/add-panel.md`（単一ファクトリ手順）、
  `docs/gpui-ui-guide.md` の DockArea 節差し替え、
  `docs/agent-api-reference.md`。
- issues: MED-APP-01 / LOW-APP-14 / LOW-APP-17 の個票に解決 PR を記録、
  `issues/README.md` の件数更新。REQ-UI-005 / REQ-UI-009 の受入条件
  チェック。GitHub #181 のクローズ。

**完了条件**

- `mise run check` / `mise run docs:check` が通る。
- doc-checklist（`docs/dev/doc-checklist.md`）の該当行を全処理。

## 単位の依存関係

```text
DOCK-1 ──▶ DOCK-2 ─────────────────┐
   └─────▶ DOCK-3 ──▶ DOCK-4 ──────┤
DOCK-5 ──▶ DOCK-6 ──▶ DOCK-7 ──────┼──▶ DOCK-8 ──▶ DOCK-9 ──▶ DOCK-10
                                   │
（DOCK-5 は独立して先行可能）───────┘
```

カットオーバー（DOCK-8）まで本体の挙動は変わらない。独立に着手できるのは
DOCK-1 と DOCK-5 の 2 本で、DOCK-1 の後は DOCK-2 と DOCK-3〜4 の 2 系統が
並走できる。

## 引き受ける issue

| ID | 内容 | 解消単位 |
|---|---|---|
| MED-APP-01 | 分離窓クローズで shell desync、dead API | DOCK-6（構造的解消） |
| LOW-APP-14 | `WindowPlacement` 未配線 | DOCK-9 |
| LOW-APP-17 | 分離窓失敗ログが `eprintln!` | DOCK-8（該当コード削除） |

未起票だが本計画で解消するもの: detach/reattach 毎の全パネル状態リセット
（DOCK-6）、detach 中レイアウト変更の巻き戻り（モデル変更で消滅）、
分離窓のダイアログ/通知不可視（DOCK-6）、パネル生成の二重登録（DOCK-8）。

## 非対象

- **ウィンドウ内フローティングパネル**。決定事項どおり detached OS
  ウィンドウに一本化する。データ構造上も表現しない（必要になったら
  `WindowLayout` の追加 variant として拡張する）。
- **ビューア専用全画面ウィンドウモード**（REQ-UI-009 の残項目）。
  全窓同型モデルの上に載る後続機能。
- **OCIO カラースペースの分離窓適用**（REQ-UI-009）。カラーマネジメント
  側の計画に属する。
- **タブの窓間 D&D のマルチディスプレイ座標系検証以上の最適化**
  （ドラッグプレビューの窓越え描画等）。まず detach → 再ドロップで成立させる。
- **gpui-component 本体からの dock 機能削除**。フォーク側の掃除は別作業。
