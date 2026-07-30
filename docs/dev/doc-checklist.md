# ドキュメント更新チェックリスト

> 索引: [`README.md`](README.md)

**「何を変えたら、どの文書を直すのか」の対応表。** 規約は
[`.agents/rules/documentation.md`](../../.agents/rules/documentation.md)、
役割分担は [`README.md`](README.md#この文書群の位置づけ)。

コードの変更と同じ変更で直す。「後でまとめて」は腐る原因なので採らない。

## コードを変えたとき

| 変えたもの | 直すもの |
|---|---|
| ノードを追加 / 削除 / `type_key` 変更 | [add-node.md](add-node.md) の手順（登録経路が変わった場合のみ）、[`../agent-api-reference.md`](../agent-api-reference.md) の `ravel-nodes` 節 |
| パネルを追加 / 挙動変更 | `../specifications/ui/<view>.md`（設計意図）、[`../ui-impl-status.md`](../ui-impl-status.md)（実装状況）、[`../specifications/ui-spec.md`](../specifications/ui-spec.md) のパネル一覧（追加時） |
| コマンド / キーバインドを追加・変更 | [`../specifications/ui/keybindings.md`](../specifications/ui/keybindings.md)、ロケール、[add-command.md](add-command.md)（経路が変わった場合） |
| 公開 API（trait / 型 / シグネチャ） | [`../agent-api-reference.md`](../agent-api-reference.md)、該当する `docs/dev/` の手順 |
| 評価・合成・キャッシュの構造 | [`../specifications/architecture.md`](../specifications/architecture.md) |
| `Document` / `Composition` / `Layer` / `Graph` の形 | [`../specifications/data-model.md`](../specifications/data-model.md)、[persistence.md](persistence.md) |
| 永続化フォーマット（`format_version` / 追加フィールド / エントリ） | [persistence.md](persistence.md)、[`../ui-impl-status.md`](../ui-impl-status.md) の永続化節、[`../specifications/data-model.md`](../specifications/data-model.md) |
| 登録経路（`processor_for_node` / `register_panels` / `for_each_command!`） | 該当する `docs/dev/` の手順（**規約で義務**） |
| アセット形式（locale / keybinding / workspace / theme） | `../specifications/ui/` の該当ファイル、[add-locale.md](add-locale.md) |
| ジオメトリ・属性の設計原則に関わる実装 | [`../specifications/procedural-geometry.md`](../specifications/procedural-geometry.md) |
| 性能を測った | [`../implementation/perf-baseline.md`](../implementation/perf-baseline.md)（warm / cold を明記する） |
| 新しい規約を決めた | [`../../.agents/rules/`](../../.agents/rules/)。grep で検出できるなら `scripts/lint-patterns.sh` にも |

## 計画・課題を動かしたとき

`backlog.md` / `roadmap.md` / `implementation/README.md` の**三者は同時に直す**。
片方だけ直さない。

| したこと | 直すもの |
|---|---|
| 計画書を新規作成 | [`../implementation/README.md`](../implementation/README.md) の In progress / Planned 表、[`../implementation/backlog.md`](../implementation/backlog.md) にセクション、[`../implementation/roadmap.md`](../implementation/roadmap.md) にフェーズ割り当て |
| 実装単位を追加 / 削除 / 分割 | 計画書と `backlog.md` の表（依存列も） |
| 単位を着手可能にした（依存が解けた） | `backlog.md` の状態記号（`⬜` → `🟡`） |
| 単位をマージした | `backlog.md` を `✅` + PR 番号（**行は消さない**） |
| 計画の全単位が完了した | 計画書を `done/` へ移動、`implementation/README.md` の Done 表へ、ソースの doc コメント参照も移す |
| フェーズの全単位が完了した | `roadmap.md` の進捗表（状態と完了日）、該当フェーズ節に実施結果 |
| 着手順の判断を変えた | `roadmap.md`（**根拠も書く**。表だけ動かさない） |
| issue を起票した | 深刻度別ファイル、[`../../issues/README.md`](../../issues/README.md) の件数とクラスタ |
| issue を計画が引き受けた | issue 個票に引受先を追記、`roadmap.md` のクラスタ行から外す |
| issue を解決した | 個票の冒頭に PR 番号（行は消さない）、`issues/README.md` の件数 |
| 要件の受入条件を満たした | `../requirements/REQ-*.md` のチェックボックス |

## 書いてはいけないこと

- **未実装の機能を実装済みとして書かない。** 仕様書に書くときは「未実装。
  担当は `<計画>`」を添える
- **同じ内容を 2 箇所に書かない。** 設計意図は `specifications/`、実装状況は
  `ui-impl-status.md`、手順は `docs/dev/`、規範は `.agents/rules/`
- **`docs/dev/` に行番号を書かない**（寿命が短くなる）。行番号が必要な精度は
  `agent-api-reference.md` 側
- **`docs/implementation/archive/` の TASK-ID を現行設計として参照しない**
  （由来の記録）
- **古い計画文書と実装が食い違うとき、文書を根拠にしない。** 実装が正。
  気づいた文書をその変更で直す
- コミットメッセージにタスク ID・issue 番号・レビュー元・エージェント名を
  入れない（明示的に要求された場合を除く）

## PR 前の最終確認

- [ ] 上の対応表で、触った範囲に該当する行をすべて処理した
- [ ] 追加した文書が索引から辿れる（[`../README.md`](../README.md) /
      [`README.md`](README.md) / `ui-spec.md` / `implementation/README.md`）
- [ ] 相対リンクが切れていない
- [ ] 未実装項目に担当計画を書いた
- [ ] ロケールを追加したなら en と ja の両方に入れた
- [ ] `mise run check`
- [ ] `ravel-review` を diff に対して流した
