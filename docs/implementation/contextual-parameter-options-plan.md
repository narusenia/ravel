# 文脈依存のパラメータ候補と出力型 実装計画

> **Status**: 未着手 — 2026-08-09

対象: `ravel-core` の `registry`（`NodeTemplate` / `Registry`）、
`ravel-ui` の `properties::node`、`ravel-app` の Properties とノードエディタ。
要件は `REQ-LAYER-005`（レイヤー間参照）と `REQ-UI-002`（パラメータ編集）。

**きっかけは `MED-APP-29`。** ただし直す対象は `layer.ref` 1 ノードではない。
足りていないのは「**そのノードが置かれた文脈から候補と型が決まる**」という
機構そのもので、`layer.ref` はそれが無いことが一番はっきり出る場所にすぎない。

## 問題

### 1. レイヤーの指定が数値スクラブになっている

`layer.ref` は参照先を `int_parameter("layer", -1)` で持つ
（`crates/ravel-core/src/registry/builtin.rs:529-540`）。Properties はこれを
`PropertyField::Int` として描くので、**−1 〜 16,777,215 の数値スクラブ**が出る。

- ユーザーはレイヤー ID を知らないし、知る画面も無い
- スクラブすると**存在しないレイヤーを指す**。指した瞬間に評価が失敗する
- `port` も自由文字列で、綴りを間違えても入力時には何も起きない

### 2. 出力の型が `port` に追随しない

同じテンプレートが出力を `DataTypeId::FRAME_BUFFER` 固定で宣言している。
参照先のポートがジオメトリでもフィールドでも**出力の型はフレームのまま**なので、
フレーム以外を参照した瞬間に**型が嘘をつく**。嘘の型は繋がるべきでないエッジを
繋がせ、評価時に初めて落ちる。

### 3. 機構がどこにも無い

上の 2 つは `layer.ref` の書き方が悪いのではなく、**宣言できる形が無い**。

| 要るもの | 今あるもの | 足りないもの |
|---|---|---|
| 候補列挙 | `Registry::param_options`（`registry/mod.rs:222`）が**テンプレート静的**な `&[String]` を返す | ドキュメントとノードの所在から候補を作る経路 |
| パラメータ間の追随 | `builtin::dependent_param_updates`（`registry/builtin.rs:225`）が `attribute.set` の `type` → `value` を書き換える | **パラメータ → 出力ポート型**の追随（`set_params` が面倒を見るのは*パラメータポート*の型だけ、`graph.rs:1094-1107`） |
| 文脈依存の候補の前例 | レイヤーの Parent ドロップダウン（`SHELL-5`、`ravel_ui::properties::layer`）が循環候補を除いて列挙する | それは**レイヤーフィールド**の経路で、ノードパラメータは通らない |

**Parent ドロップダウンを一般化するのがこの計画書。** 2 本目の機構を作らない。

## 目標アーキテクチャ

### 候補列挙: テンプレートが「文脈から引く」ことだけを宣言する

`NodeTemplate` のパラメータに、静的な選択肢の代わりに**候補の種類**を宣言できる
ようにする。

```text
ParamOptions::Fixed(Vec<String>)          // 今の param_options 相当
ParamOptions::Contextual(ContextualKind)  // 候補は文脈が決める
```

`ContextualKind` は**閉じた列挙**にする（`SiblingLayer`、`LayerOutputPort`）。
任意のクロージャを持たせない — `NodeTemplate` はシリアライズも比較もされる
データで、`ravel-core` は Document を知っていてもよいが UI は知らないという
層の分離を守る必要があるため。

解決は `ravel-core` に置く 1 つの関数:

```text
registry::contextual_options(kind, ctx) -> Vec<ParamOption>
```

`ctx` は「どの Document のどの Composition のどのネットワークか」を持つ既存の
`NetworkContext` 相当。`ParamOption` は `{ value: String, label: String }` で、
**値と表示名を分ける**（レイヤーは ID が値・名前が表示。`parse_parent_option`
が同じ問題を文字列の詰め込みで解いているので、そちらもこの型へ寄せる）。

`ravel-ui` の `properties::node` は `param_options` を呼んでいる 1 箇所
(`properties/node.rs:115`) を、文脈を受け取る形へ広げるだけにする。

### `layer` は Int をやめて String にする

候補を持てる型が `String`（`PropertyField::Enum`）しか無く、そこへ寄せる。
**レイヤー ID を数値として持ち続ける理由が無い**: `layer.ref` が欲しいのは
「同じコンポの、この名前のレイヤー」であって連番ではない。
値は `LayerId` の十進表記のままとし（`.ravprj` の移行を型変換 1 つに抑える）、
表示名はレイヤー名にする。

**フォーマット版を 1 つ上げる。** 採番は着手時に `manifest.rs` の
`CURRENT_FORMAT_VERSION` を見て**その時点の次**を取る（`DISK-*` / `AID-*` /
`PGRP-*` / `CM-2` / `WRG-4` と競る）。移行は
**ロード後の型付きパス**で行う（`.ravprj` の移行は JSON 連鎖ではなく
型付きパスという既定の方針。v5 / v6 が実例）。

### 出力型の追随: パラメータ → 出力ポート

`dependent_param_updates` の隣に、同じ形で**ポートの更新**を返す関数を置く:

```text
registry::builtin::dependent_port_updates(node, changed) -> Vec<PortRetype>
```

適用は `Graph::set_params` の中、既存のパラメータポート retype と**同じ
コミット**で行う。理由は 2 つ:

- 型が変われば繋がらなくなるエッジが出る。その破棄は既に
  `set_params` が持っている（`graph.rs:1094-1130`）ので、そこに寄せれば
  規則が 1 つで済む
- 値・ポート・エッジで **1 undo** という既存の保証をそのまま引き継げる

**参照先が解決できないときは型を変えない。** 参照が一時的に切れただけで
出力型が既定へ戻ると、繋がっていたエッジが巻き添えで消える。

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| CPO-1 | `ParamOptions` と `contextual_options`（`SiblingLayer` のみ） | — |
| CPO-2 | Properties が文脈付きで候補を引く（`layer.ref` の `layer` が Select になる） | CPO-1 |
| CPO-3 | `LayerOutputPort` 候補と `port` の Select 化 | CPO-2 |
| CPO-4 | `dependent_port_updates` と `set_params` での適用 | CPO-3 |
| CPO-5 | `layer` の Int → String 移行（フォーマット版 +1、型付きパス） | CPO-2 |
| CPO-6 | Parent ドロップダウンを `ParamOption` へ寄せる（機構を 1 本にする） | CPO-1 |
| CPO-7 | ロケール / 文書 | CPO-1〜6 |

### 単位 1: `ParamOptions` と `contextual_options`

- `NodeTemplate` の選択肢を `ParamOptions` にする。既存の宣言は
  `Fixed` へ機械的に移す（`merge` の operation、`attribute.set` の `type` など）
- `ContextualKind::SiblingLayer` を足し、解決関数を `ravel-core` に置く。
  **自分が属するレイヤーは候補から外す**（自己参照は
  `validate_layer_ref_cycles` が弾くので、そもそも出さない）
- `Registry::param_options` は `Fixed` のときだけ従来どおり答える

**完了条件**

- 既存の enum パラメータの候補が 1 つも変わらない
- 同じコンポの他レイヤーが候補として列挙され、自分は入らない
- ネットワークがレイヤーに属さないとき（コンポ直下のノード）候補は空になり、
  パニックしない

### 単位 2: Properties が文脈付きで候補を引く

- `properties/node.rs:115` の 1 箇所を、文脈を受け取る形へ広げる
- `layer.ref` の `layer` が Select として描かれる
- **候補が空のときは Select を出さず、理由を出す。** 空の dropdown は
  「壊れている」と「選ぶものが無い」の区別が付かない

**完了条件**

- レイヤーを選ぶと `layer` パラメータがその ID になる（1 undo）
- 候補に無い値が既に入っているとき、その値を選択状態として保ったまま出す
  （プロジェクトを跨いだコピペで壊さない）

### 単位 3: `LayerOutputPort` 候補と `port` の Select 化

- 選んだレイヤーのネットワークの `net.out` が持つ入力ポート名を候補にする
- `layer` が未選択・解決不能なら候補は空

**完了条件**

- レイヤーを変えると `port` の候補が入れ替わる
- 選ばれていた `port` が新しいレイヤーに無ければ、**既定へ落とさず**
  解決不能として出す（黙って別のポートを指さない）

### 単位 4: 出力型の追随

- `dependent_port_updates` を足し、`set_params` の retype と同じコミットで適用
- 型が変わって運べなくなったエッジは既存の規則で破棄される
- 参照が解決できないときは**現在の型を保つ**

**完了条件**

- `port` をジオメトリのポートへ変えると出力の型がジオメトリになる
- そのとき繋がらなくなったエッジが落ち、値・ポート・エッジで 1 undo
- 参照先を消しても出力型が既定へ戻らない

### 単位 5: `layer` の Int → String 移行

- テンプレートを `string_parameter("layer", "")` にする
- フォーマット版を 1 つ上げ、ロード後の型付きパスで `Int(n)` → `String(n)`
- 旧版の `-1`（未設定）は空文字へ

**完了条件**

- 旧 `.ravprj` が読め、参照が同じレイヤーを指したまま
- 往復して保存しても値が変わらない

### 単位 6: Parent ドロップダウンを寄せる

- `parse_parent_option` の文字列詰め込みをやめ、`ParamOption` を使う
- 循環除外は既存のまま（判定の場所は変えない）

**完了条件**

- Parent の挙動が変わらない（既存テストが 1 つも変わらずに通る）
- 値と表示名を詰め込む文字列書式がコードベースから消える

## 範囲外

- **他コンポのレイヤー参照**。`REQ-LAYER-005` はコンポ内に閉じている
- **`port` の型に合わせた入力側の検証**。エッジの受理は既存の型体系のまま
- **候補が動的に変わったときの通知**。Properties は既に再構築されるので、
  新しい観測経路を足さない
