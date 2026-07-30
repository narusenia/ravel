---
name: ravel-docs
description: >-
  Ravel リポジトリの文書を「どれが正か」を踏まえて引く。役割別検索、単位 ID /
  issue ID の解決、パネル横断の引き当て、文書間の整合性チェックを
  scripts/docs.sh 経由で行い、根拠のファイルを添えて答える。
  トリガー: 「どこに書いてある」「仕様は」「この単位は何」「issue の引受先」
  「文書の整合性」「/ravel-docs」、および実装前の調査で文書を探すとき。
---

# ravel-docs

Ravel の文書は役割で分かれており、**同じ問いに複数の文書が答える**。
どれを引用するかで答えの正しさが変わるので、役割を意識して引く。

## 役割と権威の順序

```text
規範     .agents/rules/          守るべきこと（lint と ravel-review が強制）
手順     docs/dev/               何を触るか（チェックリストが本体）
参照     docs/agent-api-reference.md, docs/gpui-ui-guide.md
設計意図 docs/specifications/    どう振る舞うべきか（ui/ はビュー別）
実装状況 docs/ui-impl-status.md  今どこまで動くか
要件     docs/requirements/      REQ-<領域>-<番号>
計画     docs/implementation/    backlog=何があるか / roadmap=どの順で / *-plan.md=設計
課題     issues/                 何が壊れているか（着手順は持たない）
```

**最優先の原則: 文書と実装が食い違うときは実装が正。** 特に
`docs/implementation/archive/` は旧 TASK-ID 世代の由来であって現行設計ではない。
古い記述を見つけたら、そのタスクの範囲内で直す
（`docs/dev/doc-checklist.md` が対応表）。

## 手順

### 1. まず `scripts/docs.sh` を使う

```bash
scripts/docs.sh <keyword>     # 役割別にグループ化して検索
scripts/docs.sh id PTR-3      # 実装単位（backlog / 計画書 / roadmap）
scripts/docs.sh id MED-CORE-09 # issue（個票 / 引受先）
scripts/docs.sh panel viewer  # 仕様 + 実装状況 + 未実装 + 関連計画 + issue
scripts/docs.sh check         # リンク切れ / 索引漏れ / issue 件数の一致
scripts/docs.sh map           # 役割の地図
```

素の `grep` から始めない。役割が混ざった結果を読むと、計画書の記述を
現状の挙動として引用する事故が起きる。

### 2. 問いの種類で読む先を決める

| 問い | 読む先 |
|---|---|
| 「今どう動く？」 | `docs/ui-impl-status.md` → 実装コード |
| 「どう動くべき？」 | `docs/specifications/ui/<view>.md` |
| 「なぜこの順番？」 | `docs/implementation/roadmap.md` |
| 「この単位は何をする？」 | 該当 `*-plan.md`（backlog は要約しか持たない） |
| 「これは既知の不具合？」 | `issues/`（深刻度別）→ 個票の引受先 |
| 「追加するには何を触る？」 | `docs/dev/add-*.md` |
| 「やってはいけないことは？」 | `.agents/rules/`（`paths` が一致するもの） |

### 3. 未実装かどうかを必ず確認する

仕様書には**未実装項目にも担当計画が併記**されている。「仕様に書いてある」を
「実装されている」と読み替えない。判定は `docs/ui-impl-status.md` と実装コード。

### 4. 答えるときは根拠を添える

`docs/specifications/ui/timeline.md` のようにファイル名まで書く。行番号は
実装コードを指すときだけ（文書は動くので）。

## 文書を更新するとき

1. `docs/dev/doc-checklist.md` の対応表で、触った範囲に該当する行を処理する
2. `scripts/docs.sh check` を通す（リンク切れ・索引漏れ・issue 件数）
3. `backlog.md` / `roadmap.md` / `implementation/README.md` の三者は同時に直す
4. 未実装を実装済みとして書かない

## 注意

- `scripts/docs.sh check` の「backlog / roadmap にファイル名で出てこない
  計画書」は**情報**。単位 ID で参照されている計画は正常
- issue 件数の照合は解決済みを含む見出し数なので、`（N件解決）` の表記まで
  含めて突き合わせる
- 新しい文書を足したら索引（`docs/README.md`、`docs/dev/README.md`、
  `docs/implementation/README.md`、`docs/specifications/ui-spec.md`）の
  いずれかに載せる。載っていないと `check` が ORPHAN として落とす
