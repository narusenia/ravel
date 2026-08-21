# パラメータのグループ（Page）実装計画

> **Status**: In progress — `PGRP-1`〜`PGRP-4` 実装済み、`PGRP-5` / `PGRP-6` 未着手
> — 2026-08-21

対象: `ravel-core` のノードテンプレート、`ravel-ui` の Properties モデル、
`ravel-app` の Properties パネルとノードエディタ。要件文書は無い（UI の構造化）。
`ofx-host-plan.md` の `OFX-5`（Parameter Suite）が同じ型を使う。

## 問題

### 1. ノードのパラメータが 1 枚の平打ちリストにしかならない

`ravel-ui/src/properties/node.rs` の `node_params_section` が、
ノードの全パラメータを 1 つの `PropertySection` に並べていた。
`PropertySection { title, fields }` という入れ物は既にあるが、
**ノードのパラメータは常にその 1 枚に入る**。

パラメータ数が 10 を超えるノード（`rasterize`、`scatter.*`、`comp.transform`）で
関係の無い項目が縦に連なり、目的の 1 つを探す作業になる。ノードが増えるほど悪化する。

### 2. ofx の Group / Page を受ける型が無い

OpenFX のプラグインは parameter descriptor で `kOfxParamTypeGroup` と
`kOfxParamTypePage` を宣言する。**これは型（プラグイン）側の宣言**で、
ホストはそれを読んで UI を組む。Ravel 側に対応する概念が無いと、
`OFX-5` でプラグインのパラメータを表示するときにグループを捨てるか、
その場しのぎの別機構を作ることになる。

### 3. ノードエディタがパラメータ値を常に描く

`node_editor/painting.rs` がノード本体にパラメータ名と値を描く。
情報量が多いノードでキャンバスが埋まるが、切る手段が無い。

## 決定事項

### グループは**ノード型**が宣言する。インスタンスに持たせるのは In ノードだけ

`NodeTemplate` に `param_groups` を足す。組み込みノードのグループ分けは
コードにあるので、**`.ravprj` には何も入らない — フォーマット変更なし**。

これは ofx と同じ所在でもある。プラグインの descriptor が Group / Page を
宣言するので、`OFX-5` は読み取った宣言をそのまま `NodeTemplate` の
`param_groups` に載せられる。ホスト側に変換層が要らない。

例外は**ネットワークインターフェースの In ノード**。ここのカスタムパラメータは
ユーザーが実行時に足すもの（`NETIF-2`）で、型は知らない。よって In ノードだけ
`Node` にグループを持ち、**ここだけがフォーマットを上げる**。

### グループは 1 階層。入れ子にしない

ofx の Page → Group は 2 階層だが、Ravel の Properties は既に
`PropertySection` の 1 階層を持っており、そこへ流し込めば表示は済む。
入れ子は「どちらの階層を畳んだのか」を状態として持つ必要が出るので、
**要求が出るまで持たない**。ofx の Page と Group はどちらも Ravel の
1 階層グループへ潰す。

### 開閉状態は UI 状態であって文書ではない

どのグループを畳んでいるかは `ui_state.json`（`VRES-3` と同じ層）に置く。
`.ravprj` に入れると、同じプロジェクトを開いた別の人の畳み方が変わる。

### グループ未指定のパラメータは先頭の暗黙グループに入る

全ノードにグループを宣言させない。宣言の無いパラメータは今までどおり
1 枚に並ぶので、**既存ノードは 1 行も変えずに現状のまま動く**。
グループは足したノードから順に効く。

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| PGRP-1 | `NodeTemplate::param_groups` と Properties の分割（挙動不変の器） | — |
| PGRP-2 | 組み込みノードへのグループ宣言 | PGRP-1 |
| PGRP-3 | 開閉状態の永続化（`ui_state.json`） | PGRP-1 |
| PGRP-4 | In ノードのインスタンスグループ（フォーマット上げ） | PGRP-1 |
| PGRP-5 | ノードエディタのパラメータ値表示トグル | — |
| PGRP-6 | ロケール / 文書 | PGRP-2〜5 |

### 単位 1: `NodeTemplate::param_groups` と Properties の分割

**実装済み。**

- `NodeTemplate` に `param_groups: Vec<(String, Vec<String>)>` を足し、
  `with_param_group` を生やす。**タプルの 1 要素目はグループ名**で、ロケール
  キーは `ravel-ui` の表示境界が `node_locale::group_key` で組む
  （`ravel-core` にロケールキーを持たせない既存の分担に合わせる。同じ名前が
  `PGRP-3` の開閉キーと `PGRP-4` のインスタンスグループ名にもなる）
- `ravel-ui/src/properties/node.rs` の `node_params_section` を
  `node_params_sections`（**`Vec<PropertySection>` を返す**）に改名。
  分割そのものは `grouped_params` の 1 本で、`param_group_titles` が
  同じ分割の「グループ名と見出し」だけを返す（`PGRP-3` のホストが使う）
- 宣言に現れないパラメータは、宣言されたグループより**前**に置く
  （既存ノードの見た目が変わらない）
- 宣言に無いキーは黙って落ち、メンバが 0 になったグループはセクションを
  作らない。同じキーを 2 つのグループが挙げたら**先に挙げた方**が勝ち、
  同じグループ名を 2 回宣言したら**1 セクションに統合**する
  （名前が開閉キーなので、同名 2 セクションは区別できない）

**完了条件**

- グループ宣言の無いノードの Properties が 1 行も変わらない（既存テストが通る）
- グループを宣言したテスト用テンプレートが、宣言順に複数セクションへ割れる
- 宣言に無いキーが暗黙グループへ落ちることのテスト

### 単位 2: 組み込みノードへのグループ宣言

**実装済み。**対象はテンプレート単位で実測した 9 つ（1 ファイルに複数
テンプレートがあるので `grep -c with_param` では出ない）。

| ノード | グループ |
|---|---|
| `attribute.set` | target / value |
| `style.stroke` | stroke / target / corner |
| `field.apply` | target / blend / scope |
| `math.remap` | input / output |
| `math.curve` | input / output / curve |
| `scene.camera` | view / lens / clip |
| `scatter.grid` | layout / source |
| `scatter.circular` | layout / source |
| `scatter.scatter` | layout / source |

計画時点の候補のうち `rasterize`（3）・`shape.*`（2〜4）・`comp.transform`
（テンプレートが無い）は 6 個に届かないので宣言していない。
`scatter.path_array` は 4 個なので同じく見送った（`source` の 3 つは兄弟と
同じ並びだが、4 行を 2 セクションに割る利得が無い）。

ロケールキーは `node.<type_key>.group.<name>` で `DISC-1` の体系に合わせる。
`ravel-ui::node_locale` のテストが **en / ja 両方の欠落**と、宣言に無い
グループの**余り**の両方を落とす。`registry/builtin.rs` 側のテストは
「1 つでも切ったら全部切る」「実在するキーだけ」「同じキーを 2 度挙げない」を
強制する。

**完了条件**

- 対象ノードの Properties がグループに割れている
- ロケールが ja / en 揃っている（`docs/dev/` のロケール手順どおり）

### 単位 3: 開閉状態の永続化

**実装済み。**

- 畳んだグループを `ui_state.json` の `collapsed_param_groups` に持つ。
  キーは `(type_key, group)` — ノードごとではない。同じ型のノードを
  選び直すたびに畳み直すのは煩わしい
- **既定は全展開**なので、書くのは「畳まれているもの」だけ。1 件も無ければ
  エントリごと書かれないので `format_version` は上げていない
  （`bpm_grid` / `loop_ranges` と同じ扱い）
- 経路は `bpm_grid` に倣った Global 1 本
  （`panels::CollapsedParamGroupsState`）。保存は `ProjectState` の
  `enqueue_save`、復元は `replace_document`
- Properties の Accordion は開いているセクションの集合しか報告せず、しかも
  行のクリックでも発火するので、`set_param_group_collapsed` が
  「変わったか」を返し、変わったときだけ再描画する。パラメータグループ
  以外のセクション（info / ports / 宣言 / 説明）は畳めるようにしていない

**完了条件**

- 畳んだ状態がアプリ再起動をまたいで残る
- `ui_state.json` を消しても既定（全展開）で起動する

### 単位 4: In ノードのインスタンスグループ

**実装済み。**

- `Node` に `param_groups: BTreeMap<String, String>`（パラメータキー →
  グループ名）を**末尾**に足した。テンプレート側の「グループ → キーの並び」
  ではなく**パラメータごとの対**にしたのは、インスタンス側には順序の出所が
  もう 1 つあるから: セクション順は最初のメンバの登場順（= ポートを足した
  順）で決まるので、改名は 1 エントリの移動、削除は 1 エントリの除去で済む。
  同期漏れの余地を作らない
- `.ravprj` フォーマットは **v11 → v12**（`migrate_v11_to_v12` は版印だけを
  進める）。`Node` にフィールドを足すと bincode の位置索引が動くので
  `JOURNAL_FORMAT_VERSION` も v10 → v11
- 書き手は `network::set_custom_port_group` の 1 本だけ。改名は
  `Graph::rename_port` がグループを連れて行き、削除は
  `network::remove_custom_port` がエントリを落とす（残すと、同名のポートを
  足し直したときに黙って元のグループへ入る）
- Properties の Ports セクションの各カスタム行に「グループ」欄が出る。
  **パラメータを持たないポート（固定ポート、wire 専用型）には欄が出ない** —
  `PortRow::group` が `None` になる
- **優先順位**: 同じ In ノードに型宣言とインスタンス宣言の両方があるときは
  **インスタンスが勝つ**（ユーザーが手で割り当てたものなので、片方ずつ
  混ぜるとどちらに従っているのか説明できない）
- **In ノード以外の非空 `param_groups`**（手編集した `.ravprj` でしか作れない）
  は**表示側が無視**し、型宣言に従う。`validate` で開けなくはしない
  — 「保存できたが開けない」を作らないため（`HIGH-26`）

**完了条件**

- In ノードのカスタムパラメータをグループに割り当てられる
- 旧バージョンの `.ravprj` が読め、グループ無しとして扱われる
- ラウンドトリップのテスト

### 単位 5: ノードエディタのパラメータ値表示トグル

- ノード本体のパラメータ名 / 値の描画を on/off する。表示単位は**全体**
  （ノードごとの設定は状態が増える割に使われない）
- `ui_state.json` に持つ

**完了条件**

- トグルでキャンバスからパラメータ行が消え、ノードの高さが縮む
- 再起動をまたいで残る

### 単位 6: ロケール / 文書

**完了条件**

- `docs/specifications/ui/` の Properties とノードエディタの記述が追随
- `docs/ui-impl-status.md` が更新されている

## 受け入れた上限（独立レビューで挙がったもの）

- **セクションの順はポートの並べ替えに追随しない。** インスタンスグループの
  セクション順は `node.parameters` の登場順で決まるが、`Graph::reorder_ports`
  はポート配列だけを並べ替えて `parameters` を並べ替えない（`main` から
  ある形で、グループ機能が「順序」に依存したことで初めて見えた）。
  パラメータ側も並べ替えるのは `PGRP-*` の外の変更なので、要求が出たら別単位
- **グループ名がロケールキーと衝突すると翻訳される。** インスタンスグループの
  名前は自由入力で、`PropertySection.title` はホストが `t!` を通す。
  `properties.section.parameters` のような**内部キーそのもの**を名前に打つと、
  その翻訳が見出しに出る。畳み判定は位置で引くので**別のセクションを畳む事故は
  起きない**（`PGRP-4` の修正）。表示だけの問題で、名前を変えれば直る
- **開閉の永続化は明示的な保存に乗る。** 折り畳みは `ui_state` を dirty にする
  だけで保存を起こさない（自動保存は `SET-9` で未実装）。「畳んで保存して
  開き直すと残る」までがこの単位の範囲

## 非対象

- **入れ子グループ**（Page → Group）。上記のとおり 1 階層へ潰す
- **ユーザーが任意のノードでグループを組む機能**。In ノード以外は型の宣言に従う。
  全ノード分の割り当てが `.ravprj` に乗る割に、得るものが薄い
- **グループ単位の一括操作**（まとめてリセット等）。要求が出てから
