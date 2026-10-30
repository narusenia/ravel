# 式エディタの補完とハイライト 実装計画

> **Status**: In progress — `EXED-1` / `EXED-2` 済（#631）。次は `EXED-3`（2026-10-30）

対象: `ravel-core` の式言語の語彙の公開（`expression/`）、`ravel-widgets` の
式エディタウィジェット、`ravel-app` の Properties（パラメータ式）、
`field.expression` ノードの式入力。要件は `REQ-CORE-014`（式言語）、
`REQ-CORE-015`（フィールド式）、`REQ-UI-002`。

前段は `done/expression-language-plan.md`（`EXPR-4`）。あちらは「式を書ける」
までで、書く体験（補完・色分け・エラー位置の表示）は **非対象のまま残った**。
本計画はそこを埋める。

## 問題

式はすでに Properties から付けられるが、入力欄は**素の 1 行テキスト**でしかない。

- 関数名・定数・変数（`frame`・`time`・`P`…）・`@attr` は **書く人が覚えている前提**。
  語彙は `Builtin::ALL`・`CONSTANTS`・`PARAMETER_VARIABLES` / `FIELD_VARIABLES` /
  `STANDARD_ATTRIBUTES` にあるが、UI から引けない
- 構文エラーは入力欄の**下に 1 行の文字列**で出るだけ。`Span` は
  `ravel_ui::properties::expression::compile_error` が返すが、
  `ExpressionDraft` / `ExpressionComponent`（`properties.rs`）は**メッセージ文字列だけ**
  を持ち、位置を捨てている
- `ravel_widgets::Input::invalid()` は存在するが式の欄では使われていない
- 字句解析器（`lexer.rs`）は `pub(crate)` で、最初のエラーで止まる。
  **編集中の壊れた式を色分けする用途には使えない**
- `WRG-1`（複数文）が入ると式は複数行になる。1 行入力のままでは持たない
- MCP の語彙説明（`ravel-cli/src/mcp.rs` の `EDIT_DESCRIPTION`）は**手書き**で、
  コアの語彙との乖離を検出するものが無い

## 決定事項

- **経路 (a) を採る（利用者決定、2026-10-29）**: 式の欄を gpui-kit の
  コードエディタ種別（`EditorState`、`LayoutMode::CodeEditor`）へ載せ替え、
  Ravel 側で `InputHighlighter` と `CompletionProvider` を実装する。
  補完ポップアップは **`ravel-widgets` に Ravel のトークンで描く**。
  gpui-component のポップアップ（`component/src/input/popovers/`）は使わない
  （`ravel-widgets/Cargo.toml` が gpui-component を意図的に除外している）。
  - 理由: `WRG-1` の複数行式にそのまま乗る。単一行 `InputState` に
    ハイライト・補完フックを足すには kit フォークの改造が要り、それを避けられる
- **`InputHighlighter` / `CompletionProvider` はコードエディタ種別でだけ動く**
  （`base/src/input/editor/highlighting.rs`、`editor/lsp/completions.rs`）。
  単一行 `InputState` の `InputEvent` は Change / PressEnter / Focus / Blur のみ
  で、色もポップアップも掛けられない。これが経路 (a) の前提
- **語彙の正はコア 1 箇所**。UI も MCP の説明もそこから引く。手書きの複製を
  増やさない
- **ハイライトはエラー許容**。壊れた式でも全トークンに分類を付ける
  （評価やコンパイルとは別の、字句だけの経路）
- **色は `ravel-widgets/src/tokens.rs` のトークン**から取る。リテラルの色を
  書かない（`ux.md` のトークン規則）
- **エラーメッセージの文字列表示は残す**。下線は追加であり置き換えではない
- **確定を妨げない**（`EXPR-4` の不変条件のまま）。補完は提案であって
  入力の検証ではない

## 失敗時の退路

**単位 1 が失敗した場合**（`EditorState` を 1 行の見た目に抑えられない、または
補完に要るキーを取れない）は経路 (b) へ退く:

- 単一行 `InputState` の上に Ravel が色付きランを描き、オーバーレイで補完を出す
- **kit フォーク（`gpui-kit-ravel`）の改造が必要**になる（テキスト描画へのランの
  差し込みと、キー入力の横取り口）
- **(b) への切り替えは新たな利用者判断を要する**。フォークの改造は
  `gpui-kit-migration-plan.md` の方針（フォークの差分を増やさない）に触れるため、
  実装者が自分で選んではいけない。単位 1 の結果を持って利用者に戻す

## 目標アーキテクチャ

```text
ravel-core::expression
  ├─ highlight_tokens(source)  -> Vec<(Span, TokenClass)>   （エラー許容）
  └─ vocabulary(scope)         -> Vocabulary                 （関数・定数・変数・属性）
              │                              │
              ▼                              ▼
ravel-widgets::expression_editor        ravel-cli::mcp
  ├─ EditorState（1 行の見た目）         └─ EDIT_DESCRIPTION の語彙節
  ├─ InputHighlighter  <- TokenClass -> tokens.rs の色   （ドリフトテストで照合）
  ├─ CompletionProvider <- vocabulary
  ├─ 補完ポップアップ（anchored、Ravel のトークン）
  └─ 診断の下線 <- Span
              │
              ▼
ravel-app::panels::properties（パラメータ式）  →  field.expression  →  SHELL-6
```

ウィジェットは `ravel-core` だけを知る（`ravel-widgets` の依存は `ravel-core` /
`ravel-i18n` / `gpui` / `gpui-base`）。`Scope` を渡すのは呼び出し側で、
ウィジェットは語彙を受け取って使うだけにする。

## 実装単位

### EXED-1 スパイク: `EditorState` を式の欄にできるか

**確かめること**（結果をこの節へ追記する。未確認の API は断定しない）:

- `EditorState` を **1 行の見た目**に抑えられるか。`auto_grow(min, max)` で
  1 行から N 行まで伸ばし、`line_number(false)` で行番号欄を消せるか
  （kit 側に両メソッドがあることは調査済み。**名前と挙動は単位 1 で確認**）
- 補完ポップアップが開いている間の **Up / Down / Tab / Enter / Escape** を
  エディタが奪う前に Ravel が取れるか。取れないなら、どの層（アクション・
  `on_key_down`・`CompletionProvider` 側）に差し込めるか
- フォーカスがエディタに残ったまま、ポップアップを別要素として描けるか
  （`gpui.md` のフォーカス所有。ポップアップが `FocusHandle` を持たない）
- 単一行の期待動作（Enter で確定、改行を入れない）を `WRG-1` 前の間だけ保てるか
- `InputHighlighter` に渡るのが**全文**か差分か。毎打鍵の再計算コスト
- Properties の行に埋めたときのレイアウト（幅・フォント・`mono`）

**完了条件**

- 上の各項目に「できる / できない / こう差し込む」を書いた結果の節
- 使い捨ての検証コードは**マージしない**（結果は文書にだけ残す）
- 失敗なら経路 (b) へ退く旨と、フォーク改造の範囲の見積りを書き、
  **利用者へ戻す**（EXED-2 以降は止める。EXED-2 は経路に依らないので
  並行して進めてよい）

#### EXED-1 結果（2026-10-29）

**結論: 経路 (a) は成立する（go）。ただし前提 2 点が違い、EXED-3 以降の設計に反映する。**
kit フォークの改造は要らない。検証は kit（rev `7eafdc2`）のソース読みと、
使い捨ての `#[gpui::test]`（`ravel-widgets` に置いて削除。マージしない）で行った。

| 項目 | 結果 |
|---|---|
| 1 行の見た目 | **`auto_grow` は使えない（できない）**。`auto_grow` は `TextareaState` にだけ在り、`EditorState` には無い（呼ぶとコンパイルエラー）。仮に在っても `LayoutMode::AutoGrow` へ差し替わり、ハイライタ・行番号は `CodeEditor` モードにしか無い。**こう差し込む**: `EditorState::new(..).line_number(false)`（`#[doc(hidden)]` だが `pub`。行番号欄が消える）のまま、高さは呼び出し側の親要素で決める。`CodeEditor` の要素は親の高さを `relative(1.)` で埋め、最小は 1 行。1 行の欄は親を 1 行高に固定し、`WRG-1` で行数を増やすときは親の高さを折り返し行数から計算する。折り返し行数の取得口は未確認 |
| Up / Down / Escape / Enter を取る | **できる**。補完メニューが開いている間、`MoveUp` / `MoveDown` / `Enter` / `Escape` は kit が `set_overlay_action_handler` のハンドラへ先に渡す（`true` を返すと消費）。メニューが閉じていれば呼ばれず従来どおり（Enter は `PressEnter`、Escape は伝播）。`present_completion_items`（開く）、`insert_completion`（確定）、`dismiss_completion_overlay`（閉じる）はいずれも `pub` |
| Tab を取る | **ハンドラ経由では取れない**（`IndentInline` は補完メニューを見ず、空白 2 個を挿入する）。**こう差し込む**: Ravel の action を `"Input"` コンテキストへ**後から** `tab` で束縛し、ラッパ要素の `on_action` で受ける。ポップアップが開いていれば消費して確定、閉じていれば `cx.propagate()` で kit の挙動へ落とせる（テストで確認）。ラッパ側の浅いコンテキストへ束縛しても負ける。束縛は全 `Input` に効くため、他の入力欄への副作用（ハンドラが無いときの扱い）は EXED-4 で確認する。キー割り当ては `assets/keybindings` と `CommandId` の経路に乗せる |
| フォーカスを残したままポップアップ | **できる**。ハンドラ経由の操作ではフォーカスは動かず、メニューが開いている間 `on_blur` は何もしない。`FocusHandle` を持たない兄弟 `div` をクリックしてもエディタのフォーカスは外れなかった（テスト）。ポップアップ内のマウス操作の挙動は未確認（実機） |
| 単一行の期待動作 | **一部できる**。`submit_on_enter(true)` で Enter は改行を入れず `PressEnter` だけを出す。ただし **Shift+Enter は改行を入れ、貼り付けも改行を取り込む**（テスト）。`WRG-1` 前は `Change` で改行を除くか、`shift-enter` / `paste` を同じ「後から束縛」で受ける必要がある。前者は undo 履歴に残る点に注意 |
| `InputHighlighter` に渡るもの | **全文 + 差分**。`update(Some(InputEdit), &Rope 全文, ..)` が打鍵ごとに 1 回、`styles(range)` は描画ごとに複数回（可視範囲）呼ばれる。`styles` は範囲を**隙間なく**覆う列を返す契約で、`highlight_tokens` が空白も含めて全バイトを覆うので合う。**こう差し込む**: `update` で `highlight_tokens` を 1 回計算して保持し、`styles` は保持した結果を範囲で切る。コストは 456 文字で 1 回あたり約 4 µs（デバッグビルド、負荷あり）で、毎打鍵の再計算は問題にならない |
| 補完の確定と undo | **できる**。`insert_completion` は 1 回の undo で戻る（`sin(a` → 確定 → `sin(abs` → undo 1 回 → `sin(a`）。`ux.md` の undo 粒度に合う |
| Properties の行でのレイアウト | **未確認（実機）**。幅・フォント・`mono` は描画が要るので headless では言えない。親要素の `text_style` を継承するかも未確認 |

**EXED-3 以降へ持ち越す判断**

- **`ravel-widgets` に `lsp-types` の直接依存が要る**（本番依存の追加）。`CompletionProvider` /
  `present_completion_items` は `lsp_types::CompletionItem` を型に持ち、`gpui-base` は `Position`
  以外を再公開しない。`Cargo.lock` には `gpui-base` 経由ですでに在り（`0.97.0`、`proposed`
  feature）、新しい crate は増えないが、`rust.md` の「本番依存の追加は先に聞く」に当たる。
  避ける手は無い（メニューを開く `open` フラグが `present_completion_items` でしか立たず、
  ハンドラはメニューが開いている間しか呼ばれないため）
- 1 行の高さは親が決める（上表）。`auto_grow` を前提にした `EXED-3` の記述は
  「親の高さを折り返し行数から出す」に読み替える
- Tab と Shift+Enter・貼り付けの横取りは同じ仕組み（後から束縛した action）に揃える

### EXED-2 コア: 語彙とエラー許容のトークン分類

- `highlight_tokens(source) -> Vec<(Span, TokenClass)>`: 壊れた入力でも止まらず、
  未知の文字は `TokenClass::Error` で 1 文字ずつ進む。`TokenClass` は
  数値・識別子（関数 / 定数 / 変数 / 属性）・演算子・括弧・空白・エラーなど（式言語にコメント構文は無いので持たない）、
  色を分ける粒度だけ持つ（字句解析器 `pub(crate)` の `TokenKind` は出さず、
  公開型を別に切る）
- `vocabulary(scope) -> Vocabulary`: `Builtin::ALL`（名前・`Arity::describe`）、
  `CONSTANTS`、`Scope::variables()` / `attributes()` から候補を組む。
  `Scope::parameter_context()` / `field_context()` を引数に取る
- `docs/agent-api-reference.md` へ追記
- **ドリフトテスト**: `EDIT_DESCRIPTION` に書かれた関数名・変数名が
  `vocabulary` と一致することを検証する（逆向きの欠落も検出する）。
  生成にするか照合にするかは実装時に決める。照合のほうが小さい

**完了条件**

- 壊れた式（未閉じの括弧、未知の文字、途中で終わる数値）の分類テスト。
  **全バイトがどれかの `TokenClass` に属する**こと（隙間・重なりが無い）
- `vocabulary` が `Builtin::ALL` の全要素を含むテスト（新しい組み込みを足して
  候補に出ない、を防ぐ）
- ドリフトテストが、語彙を 1 つ足して説明を直さないと**赤になる**ことの確認
- `ravel-core` はヘッドレスのまま（GPUI を引かない）

### EXED-3 ウィジェット: 式エディタとハイライト

依存: EXED-1（採用した経路）、EXED-2

- `ravel-widgets` の直接依存に `lsp-types` を足す（利用者承認済み、2026-10-30）。
  kit の補完 API が `lsp_types::CompletionItem` を取るため。gpui-base 経由で
  既にロックに入っている版（0.97.0）に合わせ、新しい crate は増やさない
- `ravel-widgets` に式エディタウィジェットを足す（`docs/dev/add-widget.md` の
  チェックリストに従う）。`EditorState` を EXED-1 で決めた見た目で包み、
  `InputHighlighter` が `highlight_tokens` の結果を `TokenClass → トークン色`
  へ写す
- 色は `tokens.rs` から取る。足りないトークンがあれば**トークン側へ足す**
  （ライト / ダーク両方）。リテラルの色を書かない
- `examples/gallery.rs` の `SECTIONS` に 1 行足し、両パレットで描く

**完了条件**

- 分類 → 色の写像が全 `TokenClass` を網羅する（網羅 `match`、`_ =>` を持たない）
- gallery が式エディタを両パレットで表示する
- `scripts/lint-patterns.sh` が通る（リテラル色なし）
- 通常のテキスト入力と同様に IME・選択・undo が効く（実機）

### EXED-4 補完ポップアップとキー処理

依存: EXED-3

- 識別子の前方一致で候補を絞る。関数は `Arity::describe` で署名を添える。
  `@` の後ろは属性、それ以外は関数・定数・変数
- ポップアップは `anchored()` で描く（`node_editor.rs` 既存の使い方に倣う）。
  Ravel のトークンで描き、**gpui-component には依存しない**
- キー: Up / Down で移動、Tab / Enter で確定、Escape で閉じる。**ポップアップが
  閉じているときは従来どおり**（Enter は式の確定、Escape は編集の取り消し）。
  フォーカスは常にエディタに残る
- 補完の確定は 1 つの編集（undo 1 回で戻る。`ux.md` の undo 粒度）
- キー割り当ては `assets/keybindings` と `CommandId` の経路に乗せる
  （`docs/dev/add-command.md`）。ウィジェット内で閉じた `on_key_down` の
  直書きにしない（コマンド経路の単一性）
- ロケール文字列（`t!`、`assets/locales/{en,ja}.toml`）

**完了条件**

- 候補の絞り込み・並び（完全一致 → 前方一致）をヘッドレスのテストで固定
- ポップアップ開閉中のキー割り当てが、閉時の挙動を変えないテスト
- 実機: 打鍵 → 候補 → Tab 確定 → フォーカスがエディタのまま
- モーション予算に収まる（ポップアップは出現アニメーションを持たない。`ux.md`）

### EXED-5 エラー位置の下線

依存: EXED-3

- `ExpressionDraft` / `ExpressionComponent` に `Span` を持たせ、診断として
  エディタへ渡す。`compile_error` の `Span` をそのまま使う
- 下線の色は危険トークン。メッセージの文字列表示は**下に残す**
- 位置は `Span::line_column` と同じ基準（複数行になっても行・列が合う）

**完了条件**

- 壊れた式で、下線がエラー位置に出る（実機）。直すと消える
- `Span` が保持されるヘッドレステスト（ドラフトに位置が残る）
- 既存の「確定を妨げない」テストが無改変で通る

### EXED-6 ロールアウトと文書

依存: EXED-4, EXED-5

- Properties のパラメータ式（`expression_editor_body`）を新ウィジェットへ
  載せ替える。`Input::new(state)` を使っている箇所を置き換え、
  `build_expression_inputs` と `note_expression_draft` を新しい状態型へ
- 次に `field.expression`。いまは**汎用の文字列入力**なので、フィールドスコープ
  （`FIELD_VARIABLES`）の語彙で同じウィジェットを使う。ノードのパラメータ
  種別の側で「式」を表せるかは EXED-6 の最初に確認する（足りなければ
  `add-node.md` のパラメータ種別の節に従う）
- `SHELL-6`（レイヤー殻の式入力 UI）が**このウィジェットを再利用する**ことを
  `layer-shell-wiring-plan.md` へ追記する。殻の入力が先に別の欄で作られると
  後で二重になる
- `docs/ui-impl-status.md` の「式エディタ」行、`docs/specifications/ui/` の
  該当仕様、`docs/dev/write-expression.md`、`docs/dev/add-widget.md` を更新

**完了条件**

- 既存の式の設定・編集・解除が回帰しない（`EXPR-4` の完了条件が引き続き成立）
- `field.expression` で補完候補にフィールド変数（`P` など）が出て、
  パラメータ式の変数（`frame` など）は出ない（またはその逆の境界が
  `Scope` どおり）
- `mise run check` と `mise run docs:check` が通る
- 実機確認（`docs/dev/testing.md` の MCP attach 手順とスクリーンショット）

## 検証

- EXED-2 はヘッドレス（`ravel-core`）。**分類は全入力で止まらず、隙間なし**
- EXED-3〜6 の描画とキーは実機。ヘッドレスに出せる判断（候補の絞り込み、
  キーの割り当て、`TokenClass → 色` の写像）は純粋関数へ出してテストする
- 毎打鍵の再計算（コンパイルとトークン分類）が編集の体感を落とさないことを、
  長い式（数百文字）で確認する。測るときは負荷の無い状態で

## 非対象

- **ホバーのドキュメント表示・定義へのジャンプ・リネーム**（LSP 相当の機能）。
  補完の署名表示だけを扱う
- **`WRG-1` の複数文と代入の構文**。本計画は複数行を**受けられる**器を選ぶだけで、
  構文は `wrangle-plan.md` が決める。複数文が入ったら語彙に代入先が増える
- **式の評価結果のプレビュー**（現在値のインライン表示）
- **kit フォークの変更**。経路 (a) はフォークを変えずに済むことが前提で、
  変える必要が出たら経路 (b) と同じく利用者判断
- **gpui-component のポップアップ・補完 UI の利用**
- **MCP の語彙自動生成**。EXED-2 は照合までで、説明文の生成は別の判断
