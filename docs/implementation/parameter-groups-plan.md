# パラメータのグループ（Page）実装計画

> **Status**: Planned — 2026-08-08

対象: `ravel-core` のノードテンプレート、`ravel-ui` の Properties モデル、
`ravel-app` の Properties パネルとノードエディタ。要件文書は無い（UI の構造化）。
`ofx-host-plan.md` の `OFX-5`（Parameter Suite）が同じ型を使う。

## 問題

### 1. ノードのパラメータが 1 枚の平打ちリストにしかならない

`ravel-ui/src/properties/node.rs:179` の `parameters` セクションが、
ノードの全パラメータを 1 つの `PropertySection` に並べる。
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

- `NodeTemplate` に `param_groups: Vec<(String, Vec<String>)>`（ロケールキーと
  パラメータキーの並び）を足し、`with_param_group` を生やす
- `ravel-ui/src/properties/node.rs` の `node_parameters_section` を
  **`Vec<PropertySection>` を返す形**に変える。宣言が無ければ今までどおり 1 枚
- 宣言に現れないパラメータは、宣言されたグループより**前**に置く
  （既存ノードの見た目が変わらない）

**完了条件**

- グループ宣言の無いノードの Properties が 1 行も変わらない（既存テストが通る）
- グループを宣言したテスト用テンプレートが、宣言順に複数セクションへ割れる
- 宣言に無いキーが暗黙グループへ落ちることのテスト

### 単位 2: 組み込みノードへのグループ宣言

- パラメータが 6 個以上ある組み込みノードにグループを宣言する
  （実測: `grep -c with_param` で対象を出す。計画時点の候補は
  `rasterize` / `scatter.*` / `comp.transform` / `shape.*`）
- ロケールキーは `node.<type_key>.group.<name>` で `DISC-1` の体系に合わせる

**完了条件**

- 対象ノードの Properties がグループに割れている
- ロケールが ja / en 揃っている（`docs/dev/` のロケール手順どおり）

### 単位 3: 開閉状態の永続化

- 畳んだグループを `ui_state.json` に持つ。キーは `(type_key, group)`
  — ノードごとではない。同じ型のノードを選び直すたびに畳み直すのは煩わしい

**完了条件**

- 畳んだ状態がアプリ再起動をまたいで残る
- `ui_state.json` を消しても既定（全展開）で起動する

### 単位 4: In ノードのインスタンスグループ

- `Node` に `param_groups` を足す（In ノード以外は常に空）
- `.ravprj` フォーマットを 1 つ上げる。**採番はマージ順**（`v8` が空いていれば `v8`。
  `discrete-keyframes-plan.md` / `asset-identity-plan.md` / `CM-2` と競るので、
  着手時に `manifest.rs` の `CURRENT_FORMAT_VERSION` を見て決める）
- Properties の Ports セクションからグループを編集する

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

## 非対象

- **入れ子グループ**（Page → Group）。上記のとおり 1 階層へ潰す
- **ユーザーが任意のノードでグループを組む機能**。In ノード以外は型の宣言に従う。
  全ノード分の割り当てが `.ravprj` に乗る割に、得るものが薄い
- **グループ単位の一括操作**（まとめてリセット等）。要求が出てから
