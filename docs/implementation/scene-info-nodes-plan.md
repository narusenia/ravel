# シーン情報ノード（layer.info / comp.info） 実装計画

> **Status**: Planned — 2026-07-29

対象: レイヤーとコンポジションのメタ情報をネットワークから読む手段。
関連要件: REQ-LAYER-002、REQ-LAYER-005、REQ-CORE-007。

## 問題

ネットワークは**自分が置かれた文脈をほとんど知らない**。

`net.in` が供給するのは `base_geometry` / `t` / `f` / `source` と
カスタムパラメータだけ（`crates/ravel-core/src/network.rs:29-37`）。
レイヤーの index・尺・サイズ・変換、コンポジションの解像度・fps・尺は
ネットワークから見えない。

`EvalContext`（`crates/ravel-core/src/eval.rs:105-116`）が持つのは
frame / time / fps / resolution / comp_resolution の 5 つで、レイヤーの
識別も殻の属性も入っていない。

帰結:

- 「コンポジション幅の 1/3 の矩形」のような**解像度に追従する構成**が組めない
- 「レイヤー index に応じて色相をずらす」ような**index 駆動の差分**が作れない
- 「他レイヤーの位置を見て線を引く」ができない。`layer.ref`
  （`crates/ravel-nodes/src/layer_ref.rs`）は参照先の `net.out` ポートの**値**を
  取るだけで、参照先の殻の情報は取れない

### 他コンポジションの参照は土台だけある

`precomp` は**予約されているが未実装**。

| 存在するもの | 場所 |
|---|---|
| 型キーとパラメータ名の予約 | `crates/ravel-core/src/composition/validate.rs:22-25` |
| 循環参照検出 | `validate.rs:75 validate_precomp_cycles` |
| スコープ次元の予約 | `eval.rs:182 PathSegment::Comp`（"Reserved for PreComp (v2)"） |

プロセッサは無い（`crates/ravel-nodes/src/lib.rs` の照合に `precomp` の腕が無い）。
`composition/mod.rs:959` にも「No node parameter carries a `CompId` yet」とある。

## 決定事項

### 情報は In に生やさず、専用ノードにする

`net.in` の固定ポートは「**その**レイヤー自身の殻からの注入点」という
一貫した意味を持つ。ここにレイヤー情報を 10 個以上足すと、

- ノードが縦に伸びて他の用途を圧迫する
- サブネット内の In はピン境界なので固定ポートを自動追加できず、
  内外で意味がねじれる（`network-interface-editing-plan.md` の「`f` の自動追加」参照）

`layer.ref` に足すのも避ける。あれは「参照先 Out ポートの値をその型のピンで出す」
単一責務（`layer_ref.rs:88-118`）で、情報を混ぜると `port` パラメータの意味が
二重化する。

### 構造体ポートは作らない

`LayerInfo` を 1 つのデータ型として運ぶ案は採らない。新しい `NodeData` 実装 +
シリアライズ + ポート色 + アンパックノード群が必要になり、**ポートで表現できる
ものを再発明する**。多出力は `PortRecord` が既に担っている
（`net.in` と `subnet` が同じ規約に乗っている）。

### 出力ポートは選択式にする

候補は 15 個前後あるが、固定フルセットにするとノード高さがポート数に比例して
伸び、グラフの見通しが落ちる。`network-interface-editing-plan.md` 単位 3 の
Ports セクションを流用し、**候補一覧から必要なものだけ生やす**。

型は候補側が持つので型 Select は出さない。名前も固定（自由入力させない）。
既定は `index` / `size` / `local_t` の 3 つ。

これにより **本計画は `network-interface-editing-plan.md` の単位 1〜3 に依存する**。

### `-1` で自分、それ以外で他レイヤーを指す

`layer` パラメータの規約を `layer.ref`（`layer_ref.rs:45`、`builtin.rs:348`）と
揃える。1 ノードで「自分の情報」と「他レイヤーの情報」の両方を賄い、
レイヤー選択ウィジェットも共有する。

`comp.info` の `comp` パラメータも同様に `-1` = 現在のコンポジション。

### 情報ノードは対象ネットワークを評価しない

`layer.info` は Document の殻フィールドを読むだけで、参照先のネットワークを
pull しない。よって `layer.ref` のような評価再帰も、グラフ上の循環も発生しない。
`comp.info` も同様なので、**他コンポジションの情報は `precomp` 本体より先に
実装できる**（`PathSegment::Comp` も循環検出も不要）。

### 殻フィールドの変更を invalidation に載せる

現状、殻の transform / 時間配置 / opacity の編集は
`InvalidationHint::None` でコミットされる（`crates/ravel-app/src/panels/properties.rs:839-842`。
`blend_mode` / `solo` / `muted` / `adjustment` だけが `Structural`）。
殻の値がグラフ評価の入力になっているのは現状 `custom.*` だけで、それは
`Params([in_node])` を出している（`:834-837`）。

情報ノードを入れると**殻の任意フィールドがグラフの入力になる**ため、
ヒントを追加する。

```rust
pub enum InvalidationHint {
    None,
    Params(Vec<NodeId>),
    Shell { comp: CompId, layer: Option<LayerId> },   // 追加
    Structural,
}
```

`merge` の強さは `Structural > Shell > Params > None`
（`crates/ravel-core/src/runtime/eval_service.rs:46-64` を拡張）。
`Shell` 同士は comp / layer の集合を統合する。

**殻編集を一律 `Structural` に格上げする案は採らない。** transform スクラブ中に
毎フレーム全キャッシュ破棄 + 全パイプライン再構築になり、RESP-3（#193）で
直したのと同じ失敗を繰り返す。

### 殻バインド経由の循環を検出する

パラメータはノード出力で駆動できる（REQ-LAYER-004。
`Layer::duplicate_with_fresh_ids` が shell binding を remap している。
`composition/mod.rs:236`）。したがって、

```text
A.transform ← A 内のノード ← layer.info(B).position
B.transform ← B 内のノード ← layer.info(A).position
```

が組める。これは**グラフ内の循環ではない**ので、`add_edge` の循環検出
（`graph.rs:661-668`）にも `validate_layer_ref_cycles` にも
`EvalScope` の再入ガードにも掛からない。殻バインドを辺に含めた循環検出が必要。

## 目標アーキテクチャ

```text
Document
  └ Composition ── layers
                     └ Layer (殻)
                          └ network
                               ├ layer.info(layer = -1 | LayerId)
                               │    └ 殻フィールドを読む（評価しない）
                               └ comp.info(comp = -1 | CompId)
                                    └ Composition フィールドを読む

InvalidationHint::Shell { comp, layer }
  └ ワーカーが info ノードを走査し、該当するものだけ dirty にする
```

## 実装単位

### 単位 1: `InvalidationHint::Shell`

- ヒント追加と `merge` の強さ順の拡張
- 殻編集（transform / timing / opacity / audio）の発行元を `Shell` に切り替える
- ワーカー側で `Shell` を受けたとき、対象 comp / layer を参照する情報ノードを
  走査して dirty にする（この時点では情報ノードがまだ無いので、走査は空でよい。
  挙動不変のまま経路を通す）

**完了条件**

- `merge` の強さ順のテスト（`Structural` が吸収、`Shell` 同士が統合、
  `Shell` が `Params` を吸収しない）
- 殻の transform 編集で `Structural` が発行されず、GPU パイプラインの
  再コンパイルが起きないことのテスト（RESP-3 の回帰を守る）

### 単位 2: `layer.info`

- 候補ポート表（下記）と、`-1` = 自分の解決
- 参照先レイヤーのローカル時間は `layer.ref` の写像を共有する
  （`layer_ref.rs:77-78`。コンプ時間へ戻して対象のローカル時間へ入れ直す）
- 表示区間外の扱いは `layer.ref` と揃える（区間外は型ゼロ。`:81-83`）

| ポート | 型 | 内容 |
|---|---|---|
| `name` | Text | レイヤー名 |
| `index` | Scalar | コンポジション内の並び順 |
| `start_frame` / `in_frame` / `out_frame` | Scalar | 殻の時間配置 |
| `duration` | Scalar | `out_frame - in_frame` |
| `size` | Vec2 | レイヤーの基準サイズ（コンプ解像度） |
| `local_t` / `local_f` | Scalar | 対象のレイヤーローカル時間・フレーム |
| `position` / `scale` / `anchor` | Vec2 | 殻の変換（**ローカル値**） |
| `rotation` | Scalar | 殻の回転（ラジアン） |
| `opacity` | Scalar | 殻の不透明度 |
| `world_position` / `world_scale` / `world_rotation` | Vec2 / Vec2 / Scalar | 親チェーン適用後 |

ローカルと world は**必ず別ポートにする**。親付けが効くかどうかが暗黙になると
事故る。

**完了条件**

- `-1` で自分、`LayerId` で他レイヤーを読むテスト
- 親を持つレイヤーで local と world が異なり、world が既存の合成行列と一致する
  テスト
- 対象が表示区間外のとき型ゼロを返すテスト
- 殻の transform を変更したとき、`Shell` ヒント経由で値が更新されるテスト
- 対象レイヤーが存在しないときのエラーメッセージが対象 id を含むテスト

### 単位 3: `comp.info`

- `-1` = 現在のコンポジション、それ以外は `CompId`
- 出力候補: `resolution`(Vec2) / `frame_rate`(Scalar) / `duration_frames`(Scalar) /
  `comp_t` / `comp_f`(Scalar) / `background`(Color) / `layer_count`(Scalar) /
  `name`(Text)
- **他コンポジションを参照する場合の時間基準を明示する**。参照先の fps が
  異なるとき、`comp_t`（秒）は現在の評価時刻をそのまま秒で返し、
  `comp_f` は参照先の fps で割り直したフレーム番号を返す。
  秒を正とする（`FrameRate` は有理数で 30000/1001 を含むため、フレーム番号の
  往復は情報を失う）

**完了条件**

- 現在コンプと他コンプの両方を読むテスト
- 参照先が 30000/1001 fps、現在が 30/1 のとき `comp_f` が期待値になるテスト
- 存在しない `CompId` でエラーになるテスト

### 単位 4: 情報ノードのポート選択 UI

- `network-interface-editing-plan.md` 単位 3 の Ports セクションを流用し、
  候補一覧からのチェック追加に切り替える（型 Select と名前入力は出さない）
- 既定 3 ポートでノードを生成する

**完了条件**

- 候補一覧からポートを追加・削除でき、エッジが保存される `ravel-ui` テスト
- 候補外の名前を生やせないテスト

### 単位 5: 殻バインドを含む循環検出

- 殻の node-output バインドを辺として扱い、`layer.info` の参照辺と合わせて
  循環を検出する。既存の `validate_layer_ref_cycles` / `validate_precomp_cycles`
  と同じ層（`composition/validate.rs`）に置く
- 検出は編集時（コミット前）とロード時。ロード時に見つかったらバインドを外して
  警告する（ファイルを開けなくしない）

**完了条件**

- 2 レイヤー間の相互参照が検出されるテスト
- 3 レイヤーを跨ぐ循環が検出されるテスト
- 循環でないバインド（A → B の一方向）が拒否されないテスト

### 単位 6: レジストリ / ロケール / 文書

- テンプレート登録（`registry/builtin.rs`）、プロセッサ登録
  （`ravel-nodes/src/lib.rs`）、ポート名と候補ラベルのロケール
- `docs/agent-api-reference.md` に 2 ノードと `InvalidationHint::Shell` を記載
- REQ-LAYER-002 / 005 に情報ノードの位置づけを追記

## 検証

- `mise run check`
- 情報ノードのテストは `ravel-core` / `ravel-nodes` の headless テストで書く
  （Document を組んで評価する形。ウィンドウ不要）
- `Shell` ヒントの効果は `ravel-core` の `EvalService` レベルで検証する

## 非対象

- **`precomp` 本体**（他コンポジションの**出力**を取り込む）。`PathSegment::Comp`
  と循環検出の実装が必要で、REQ-LAYER-005 で v2 とされている。`comp.info` は
  その足がかりだが、本計画では情報の読み出しに限る
- **式（Lua）からの情報参照**。REQ-LAYER-004 で v2、REQ-CODE-001 待ち
- **レイヤー名の文字列による動的参照**。REQ-LAYER-005 で v2
- **Text 型ポートの下流利用**。`PLAIN_TEXT`（`id.rs:222`）は型として存在するが、
  文字を消費するノードは `typography-plan.md` の担当
