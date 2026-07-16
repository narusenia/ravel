# GPUI Command / Focus リファクタ計画

## 背景

現在の GPUI 実装では、ショートカット、ネイティブメニュー、パネル固有のキー処理が複数の経路を通っている。

主な問題は以下の通り。

- 同じコマンドを App-level と Workspace-level の両方で処理している
- `PendingCommand(Option<CommandId>)` を Global に保存し、次回の `render()` で処理している
- `render()` 内でコマンド実行、Global 更新、レイアウト再構築、focus 変更を行っている
- Workspace が毎回の `render()` で自身へ focus を戻している
- 各パネルが独立した `FocusHandle` と `FocusedPanelGlobal` を管理している
- Node Editor の Copy/Paste/Delete などが中央の Command 経路を通らず、`on_key_down` で直接処理されている
- 一回限りのイベントに `Global<Option<Event>>` を使用している

このため、入力部品とグローバルショートカットの競合、コマンドの上書き、二重実行、focus 状態と `FocusedPanelGlobal` の乖離が発生しやすい。

## 目的

- 1 操作につきコマンドを必ず 1 回だけ実行する
- ショートカット、メニュー、ボタンを同じ経路へ統合する
- focus の変更を `render()` から分離する
- パネル固有操作を中央の Command 体系へ載せる
- コマンド処理を可能な限り headless に保ち、テスト可能にする
- 将来 Slint へ移行する場合も Command 層を再利用できる構造にする

## 目標アーキテクチャ

```text
KeyBinding / Native Menu / Button
                │
                ▼
           GPUI Action
                │
                ▼
       Single Command Dispatcher
                │
                ▼
        Focused Command Target
        ├─ Text Input
        ├─ Node Editor
        ├─ Timeline
        ├─ Properties
        └─ Workspace / AppShell
                │
                ▼
          Command Outcome
```

## Phase 0: 不具合の固定と観測

### 作業

現在のショートカット配送を追跡できる一時的な command tracing を追加する。

最低限、以下を記録する。

- input source: keybinding / menu / button
- action type
- focused window
- focused panel
- 実際に処理した handler
- `CommandOutcome`
- execution count

以下の条件をテストマトリクスにする。

- Workspace、Node Editor、Timeline、Properties に focus がある状態
- メインウィンドウと detached window
- `Cmd+Z` / `Cmd+Shift+Z`
- `Cmd+C` / `Cmd+V` / `Cmd+X`
- Delete / Backspace
- `Alt+1` から `Alt+6`
- メニューから同じ操作を実行
- macOS の Cmd と Windows/Linux の Ctrl
- ドラッグ操作中と通常状態

### 完了条件

- 現在の失敗条件を最低 1 つ再現できる
- 「未配送」「上書き」「二重実行」「誤った focus target」を区別できる
- リファクタ前後で比較できる再現手順が残っている

### 実施結果

- tracing: `crates/ravel-app/src/trace.rs`。`CommandTrace` Global に配送ステップを
  記録し、`RAVEL_LOG=ravel::command_trace=debug` でログにも出力する。
- 再現テスト: `crates/ravel-app/tests/command_dispatch_repro.rs`（`gpui::test`）。
- 確認された故障モード:
  - 上書き: 2 つの App-level Action が `PendingCommand(Option)` を上書きし、
    先のコマンドが実行されずに消失する。
  - 未配送: メインウィンドウでは Workspace-level `on_action` が Action を
    排他的に消費し、`CommandOutcome::Delegate` を破棄する。EditUndo を
    `PanelUndoRedo` に変換する App-level 経路は実行されないため、
    キーボードの Undo/Redo はメインウィンドウでは機能していない。
  - 誤った focus target: `RavelWorkspace::render()` が毎フレーム自身へ focus を
    戻し、パネルが取得した focus を奪う。

## Phase 1: Action 定義の一本化

### 主な対象

- `crates/ravel-ui/src/command.rs`
- `crates/ravel-app/src/workspace.rs`
- `assets/keybindings/default.toml`

### 作業

現在、Command と GPUI Action の対応は以下の場所で重複している。

- `actions!` 宣言
- App-level action 登録
- KeyBinding 変換
- Menu 変換
- Workspace の `on_action`

Command と GPUI Action の対応を一つの定義から生成するか、少なくとも一つの adapter に集約する。

パネル固有操作として、必要に応じて以下を `CommandId` に追加する。

- `EditDelete`
- `EditDuplicate`
- `ViewFit`
- `PlaybackToggle`

Copy/Paste/Undo などはグローバル操作ではなく、「現在の対象へ送る Edit Command」として扱う。

### 完了条件

- Command 追加時に編集する対応表が 1 か所になる
- KeyBinding と Menu が同じ GPUI Action を生成する
- 全 `CommandId` の Action 対応漏れを検出するテストがある
- 既存のキーバインド TOML 形式を維持する

## Phase 2: `PendingCommand` と render 副作用の廃止

### 主な対象

- `crates/ravel-app/src/workspace.rs`
- `crates/ravel-app/src/main.rs`

### 作業

以下を廃止する。

```rust
pub struct PendingCommand(pub Option<CommandId>);
```

現在 `RavelWorkspace::render()` にある以下の処理を、Action callback または専用メソッドへ移す。

- `PendingCommand` の取得とクリア
- Undo/Redo signal の生成
- `AppShell::handle_command`
- `CommandOutcome` の適用
- レイアウト再構築の開始
- focus 変更

Command は Action callback で一度だけ dispatch する。

```text
GPUI Action
    │
    ▼
dispatch_command(command, window, cx)
    │
    ├─ Focused Panel
    └─ AppShell
```

レイアウト再構築が次フレームまで待つ必要がある場合も、コマンドそのものはキューに保存せず、明示的な `needs_layout_rebuild` 状態だけを保持する。

### 完了条件

- `PendingCommand` Global が存在しない
- `render()` が基本的に UI 構築だけを行う
- 複数コマンドが `Option` の上書きによって消失しない
- 同じ Action を App-level と Workspace-level で二重処理しない
- 1 Action が 1 回だけ dispatch されるテストがある

## Phase 3: Focus 所有権の整理

### 主な対象

- `crates/ravel-app/src/workspace.rs`
- `crates/ravel-app/src/panels/mod.rs`
- `crates/ravel-app/src/panels/node_editor.rs`
- `crates/ravel-app/src/panels/timeline.rs`
- `crates/ravel-app/src/panels/properties.rs`

### 作業

`RavelWorkspace::render()` から以下の処理を削除する。

```rust
self.focus_handle.focus(window, cx);
```

Workspace は起動時、または明示的に Workspace の空白部分をクリックしたときだけ focus を取得する。

各パネルは以下の規則に統一する。

- パネルをクリックしたときに自身の `FocusHandle` へ focus を移す
- `on_focus` / `on_blur` を使ってパネルの focus 状態を同期する
- `render()` 中には focus を変更しない
- 子の入力部品が focus を持つ場合は、その focus を奪わない

`FocusedPanelGlobal` はクリック履歴ではなく実際の focus に追従させる。可能であれば最終的に Global を廃止し、GPUI の Action 伝播と focus 階層を Command Target 判定に利用する。

`AppShell::focused_panel` は detach/reattach の対象判定に必要なため、パネルの `on_focus` を起点に同期する。

### 完了条件

- `render()` によって focus が変化しない
- 子の入力部品が持つ focus が維持される
- パネル切り替え後の Command Target が実 focus と一致する
- detached window でも対象パネルを正しく判別できる
- focus 表示とコマンド配送先が一致する

## Phase 4: パネル固有コマンドの Action 化

### 主な対象

- `crates/ravel-app/src/panels/node_editor.rs`
- `crates/ravel-app/src/panels/timeline.rs`
- `crates/ravel-app/src/panels/properties.rs`
- `crates/ravel-app/src/panels/mod.rs`

### 作業

Node Editor の生の `on_key_down` から以下を除去し、GPUI Action として処理する。

- Copy
- Paste
- Duplicate
- Delete
- Fit View

キーそのものを判定するのは、文字入力や一時的なドラッグ操作など、Action にしにくい低レベル操作だけに限定する。

Edit Command は focus 階層で最も近い handler が処理する。

```text
EditCopy
  ├─ Text Input が focus 中   -> テキストをコピー
  ├─ Node Editor が focus 中  -> 選択ノードをコピー
  └─ 未対応パネル             -> Workspace へ伝播
```

Undo/Redo についても `PanelUndoRedo(Global<Option<_>>)` を廃止し、Action を focus 対象へ直接配送する。

### 完了条件

- Node Editor が Cmd/Ctrl 修飾キーを直接判定しない
- Edit Command が focus 階層に従って処理される
- Undo/Redo が Global signal を経由しない
- メニューの Copy/Paste とショートカットが同じ結果になる
- 未対応の Action が必要に応じて親へ伝播する

## Phase 5: パネル間 Global signal の整理

この Phase はショートカット安定化後の保守性改善として、Phase 0 から Phase 4 とは分けて実施する。

### 主な対象

- `SelectedPropertiesTarget`
- `PropertyChanged`
- `PanelUndoRedo`
- `FocusedPanelGlobal`

### 作業

状態と一回限りのイベントを分離する。

- 選択対象: 共有状態または Entity
- 値変更: `EventEmitter` / `Subscription`
- Undo/Redo: GPUI Action
- Focus: GPUI focus event

特に `PropertyChanged` のような一回限りのイベントを Global 状態として保持しない。

### 完了条件

- `Global<Option<Event>>` パターンがなくなる
- イベントが再 render で再処理されない
- Properties と Node Editor の依存方向が明確になる
- 状態の所有者とイベントの購読者がコードから判別できる

## Phase 6: テストと回帰確認

### 追加するテスト

- `CommandId` と GPUI Action の全対応
- 1 Action = 1 dispatch
- Menu と KeyBinding の同値性
- focus target 別の Copy/Paste/Undo
- 入力部品が持つ Edit Command の優先
- detached window での配送
- 未対応 Command の伝播
- パネル切り替え後の配送
- TOML キーバインド再読み込み後の配送
- レイアウト再構築後も handler が重複しないこと

GPUI 統合テストが難しい部分は、Command Router を headless に切り出して通常の Rust テストで検証する。GPUI 固有の focus/action 伝播は、可能な範囲で `gpui::test` を使用する。

### 回帰確認

- Workspace 全テスト
- GPU テスト
- macOS 手動スモークテスト
- Windows/Linux の primary modifier 変換確認
- メインウィンドウと detached window の確認

## 非対象

このリファクタでは以下を変更しない。

- Node Editor の描画方式
- Timeline の描画方式
- Dock レイアウト仕様
- Graph / Evaluator
- プロジェクトファイル形式
- キーバインド TOML 形式
- UI デザイン
- GPUI から Slint への移行

## 実装単位

レビューと切り戻しを容易にするため、以下の単位に分ける。

1. 再現テストと command tracing
2. Command / Action 対応表の集約
3. `PendingCommand` と render 内 command dispatch の廃止
4. Workspace とパネルの focus 所有権整理
5. Node Editor の Edit Command Action 化
6. Undo/Redo Global signal の廃止
7. パネル間 Global event の整理
8. 統合テストと tracing の削除または通常ログへの縮小

各単位で既存テストを通し、挙動変更と構造変更を同じ差分に詰め込みすぎない。

## GPUI 継続の判定基準

以下を満たせれば、GPUI 継続を合理的と判断する。

- ショートカットが全 focus 状態で安定する
- 1 操作が必ず 1 回だけ実行される
- `render()` にコマンド処理や focus 変更がない
- 新しい Command の追加箇所が一つに集約されている
- パネル固有操作の自動テストが書ける
- detached window でも Action が安定する

## Slint 移行の判定基準

リファクタ後も以下が残る場合は、Slint の縦切り試作へ進む。

- GPUI の focus/action 伝播自体が再現性なく失敗する
- ネイティブメニューだけ別のコマンド経路を必要とする
- detached window で Action が安定しない
- 入力部品とグローバルショートカットを両立できない
- GPUI または `gpui-component` の更新で同じ領域が繰り返し破壊される
- Command/Focus を整理しても UI 実装の保守コストが十分に下がらない

## 推奨実施範囲

Phase 0 から Phase 4 を最初のリファクタ・マイルストーンとする。Phase 5 は動作安定後に別マイルストーンとして実施する。

Phase 0 から Phase 2 で作成する Command の整理とテストは、途中で Slint 移行へ切り替えた場合も再利用する。

## 実施状況（第1マイルストーン）

Phase 0 から Phase 4 および Phase 6 の自動テスト部分は実装済み。PR は
実装単位ごとにスタックしている。

- Phase 0: PR #42 — tracing（`crates/ravel-app/src/trace.rs`）と再現テスト
- Phase 1: PR #43 — `for_each_command!` 単一対応表、CommandId 4 種追加
- Phase 2: PR #44 — `PendingCommand` 廃止、`dispatch_command()` 一本化、
  `MainWorkspace` Global 経由の App-level フォールバック
- Phase 3: PR #45 — focus 追跡を `on_focus_in`/`on_focus_out` へ移行、
  `render()` の focus 変更を廃止
- Phase 4: PR #46 / #47 — Node Editor の Edit Command Action 化
  （`NodeEditor` key context）、`PanelUndoRedo` Global 廃止
- Phase 6: PR #48 — TOML 再読み込み・レイアウト再構築・パネル切替の
  回帰テスト、tracing の上限付き通常計測への縮小

残作業:

- macOS 手動スモークテスト（Node Editor の Action 実行は GPU が必要で
  自動テスト対象外）
- Windows/Linux の primary modifier 変換確認
- Phase 5（パネル間 Global signal の整理）は別マイルストーン
