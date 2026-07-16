---
name: ravel-review
description: >-
  Ravel リポジトリ固有の diff レビュー。PR 作成前に必ず実行する。
  .agents/rules/ の文脈依存不変条件（render 純粋性、focus 所有権、Command
  経路単一性、Global 用法、コア層分離）を検査手順として辿り、可能なら codex
  に並列レビューさせ、所見を突き合わせて判定する。PASS 時に review-gate
  マーカーを記録する（gh pr create はマーカーがないとブロックされる）。
  トリガー: "/ravel-review"、PR 作成前、「レビューして」（Ravel リポジトリ内）。
---

# ravel-review

Ravel の diff を、機械 lint が見えない文脈依存ルールまで含めてレビューする。
最後まで完走し、判定（PASS / FAIL + 所見一覧）を報告すること。

## 1. スコープ確定

- 引数があればその対象（ブランチ、コミット範囲、PR 番号）。
- なければ `git merge-base HEAD <base>` から HEAD までの diff。base は PR の
  base ブランチ、なければ `main`。
- `git diff --stat <range>` で対象ファイルを列挙し、`.agents/rules/` のうち
  `paths` frontmatter が一致するルールファイルを全て読む。

## 2. 機械検査ベースライン

```bash
mise run lint:patterns
```

失敗したらこの時点で FAIL。`scripts/lint-patterns.allow` への追記が diff に
含まれる場合、正当化コメントと該当ルール文書の裏付けがあるか確認する。

## 3. 文脈依存チェックリスト

対象ファイルの種別ごとに、diff の該当箇所を実際に読んで確認する。
推測で PASS にしない。各項目、違反の疑いがあれば file:line で記録する。

### render 純粋性（`impl Render` を触る全 diff）

- `render()` 内に focus 変更（`.focus(`）がない
- `render()` 内にコマンド実行・`handle_command`・Global 書き込みがない
- `render()` 内の状態変化は表示用フラグの消費（`needs_full_rebuild` 型）のみ

### focus 所有権（panels / workspace を触る diff）

- 新パネルは `track_panel_focus()` で `FocusedPanelGlobal` を同期している
- `on_mouse_down` での手動 focus 取得・`FocusedPanelGlobal` 書き込みがない
- 子入力部品から focus を奪う経路を追加していない

### Command 経路（コマンド・ショートカット・メニューを触る diff）

- 新コマンドは `CommandId` + `for_each_command!` テーブルのみで追加
- 実行経路が `dispatch_command()` または panel の `on_action` に限られる
- パネル固有ショートカットは key context 付き `KeyBinding`
- メニュー・キーバインド・ボタンが同じ Action を生成する

### Global / イベント（`set_global` / `observe_global` / `Subscription` を触る diff）

- 新 Global は耐久状態のみ（one-shot イベントでない）
- コンポーネントイベントは `EventEmitter` + `Subscription`
- `Subscription` が観測者の寿命まで保持されている

### コア層分離（ravel-core / ravel-ui を触る diff）

- ravel-core に UI 依存が入っていない
- Graph 変異が新 `Graph` を返す不変モデルを維持
- Document スナップショット undo の原子性を壊していない
- ブロッキング I/O・重い処理が UI スレッドに乗っていない

### テストとドキュメント

- 新挙動に headless テストがある（GPU 必須なら手動確認手順が PR に明記）
- 挙動・構成変更が docs / locale / keybindings 資産に反映されている
- 横断変更なのに `docs/implementation/` の計画書がない場合は FAIL
  （AGENTS.md の Design gate）

## 4. codex 並列レビュー（可能な場合）

`HERDR_ENV=1` かつ `codex` が利用可能なら、Herdr ペインの codex に同じ diff
範囲とこのチェックリストを渡して独立レビューさせる（`codex --yolo`、
「report findings only, do not edit files」を明示）。自分の所見と突き合わせ、
codex のみが挙げた所見は必ずコードで裏取りしてから採否を決める。
利用不可ならスキップし、判定にその旨を記す。

## 5. ファクトチェックと判定

- 全所見について、該当コードを再読して偽陽性を除外する。
- 所見は `file:line — ルール名 — 内容 — 修正案` の形式で列挙。
- 重大度: FAIL（不変条件違反・設計ゲート欠落）/ WARN（要説明・軽微）。
- FAIL が 0 なら PASS。

## 6. ゲート記録

PASS の場合のみ:

```bash
bash scripts/review-gate.sh --mark
```

これが `gh pr create` を許可するマーカーになる（HEAD ツリーに紐づくため、
レビュー後にコミットを積んだら再レビューが必要）。FAIL の場合はマーカーを
記録せず、所見の修正 → 再実行を促す。
