---
name: plane-sync
description: >-
  docs/implementation/backlog.md の実装単位と issues/ を Plane（workspace
  `ravel` / project `RAVEL`）へ一方向に写す。scripts/plane-sync.py が差分を
  出し、このスキルが Plane MCP で反映する。リポジトリが正本で、Plane 側の
  手編集は次の同期で上書きされる。
  トリガー: 「Plane に同期」「Plane を更新」「/plane-sync」。
---

# plane-sync

**向きはリポジトリ → Plane だけ。** Plane で直した内容は戻ってこないし、
同じ項目が次に同期されたときに上書きされる。直すならリポジトリを直す。

## 何がどう写るか

| 元 | Plane の state | label | module |
|---|---|---|---|
| 単位 🟡 | Todo | `unit` | backlog の `###` 節（`done/` は除いた名前） |
| 単位 ⬜ / ❓ | Backlog | `unit`（❓ は `判断待ち` も） | 同上 |
| 単位 ✅ / ❌ | Done / Cancelled | `unit` | 同上 |
| issue（open） | Todo | `issue` + 種別 | なし |
| issue（`closed/`） | Done | `issue` + 種別 | なし |

- 照合キーは `external_source=ravel-repo` と `external_id=<単位 ID または issue ID>`
- issue の priority は深刻度から付く（critical → urgent、high / medium / low はそのまま）
- 一度も写していない単位が ✅ / ❌ のときは作らない（完了済みを遡って入れない）
- リポジトリの誤採番で同じ ID が 2 つあるときは、glob 順で後ろのほうの external_id が `<ID>~2` になる

## 前回の状態

`$XDG_CACHE_HOME/ravel/plane-sync.jsonl`（既定は `~/.cache/ravel/`）に、項目ごとの
Plane の id・内容のハッシュ・module を持つ。追記専用で、同じ external_id は最後の行が勝つ。リポジトリの外にあるので、どの
worktree から走らせても同じものを見る。**消えても重複は作らない**: 全件が差分に
出て、手順 4 で external_id から既存の Plane の項目を見つけて更新する。ただし
全件分の呼び出しがかかり、**完了済みの単位（✅ / ❌）は差分に出なくなる**ので、
Plane 側でそれらが Todo / Backlog のまま残っていないかを別途確かめる。

**同期は同時に 1 本だけ。** 2 本走ると、どちらも「未作成」と判断して同じ項目を
2 回作りうる（Plane 側に external_id の一意制約は無い前提で扱う）。

## 手順

1. `scripts/plane-sync.py selftest`。落ちたら同期しない（抽出が壊れている）
2. `scripts/plane-sync.py plan` で差分を得る。`ops` が反映する項目、
   `gone_from_repo` はリポジトリから消えた項目
   - **書き込む前に、件数（作成見込み / 更新）と `gone_from_repo` を利用者に示して
     確認を取る**。`ops` が空なら「差分なし」と言って終わる
3. ID を引く（1 回だけ）: `project list` で identifier `RAVEL` の id、`state list`、
   `label list`、`module list`。`ops` が要る label / module が無ければ作る
4. `ops` を 1 件ずつ処理する。**`show` は 1 件ずつ呼ぶ。** `export` を丸ごと読むと
   本文で文脈が溢れ、エージェントが止まる
   1. `scripts/plane-sync.py show <external_id>` で中身を得る
   2. `plane_id` が null なら `workitem list`（external_source=`ravel-repo`、
      external_id）で既存を探す。該当なしは 404 で返る
   3. 既存があれば `workitem update`、無ければ `workitem create`。name・state・
      labels・priority・description_html・external_source・external_id を渡す
   4. `module` があれば `module manage_workitems` で追加する。`old_module` があれば
      そこから外す
   5. `scripts/plane-sync.py ack <external_id> <plane_id> <hash>`。hash は 4.1 の
      `show` が出した値を渡す（今のリポジトリでなく、送った内容を記録するため）。**成功した直後に毎回**
      打つ。途中で止まっても、次の `plan` は残りだけを出す
5. `gone_from_repo` は消さずに報告だけする（ID の付け替えか削除かは人が判断する）
6. 報告: 作成数、更新数、失敗した external_id とエラー文、`gone_from_repo`

## 委譲するとき

`ops` が 20 件を超えるなら、15 件以下ずつに分けてサブエージェントへ渡す。
渡すのは external_id の一覧と手順 4 だけにする。各エージェントは自分の分だけ
`show` と `ack` を回す。`ack` は 1 行の追記なので並行で走らせてよい。
同じ項目を 2 つのエージェントに渡さないこと（同じ項目を 2 回作りうる）。

## 足りないもの

- Plane からリポジトリへは戻さない
- 消えた項目を Plane から消さない
- 本文の md → HTML は手書きの変換（入れ子のリストは平らになる）
