# インスタンスをピースとして配り分ける 実装計画

> **Status**: 未着手 — 2026-09-17

対象: `ravel-core` の `geometry`（`ops` とインスタンスの標準属性）、
`ravel-nodes` の `scatter`、`ravel-core` の `registry`、ロケールと文書。
要件は `REQ-CORE-010`（属性）と `REQ-MOGRAPH-001`、`REQ-MOGRAPH-004`
（プロシージャルタイポグラフィ）。

**きっかけは「`text.layout` を `scatter.*` に繋ぐと、1 まとまりのテキストが
各点にクローンされる。1 文字ごとに配りたい」。** 調べたところ、配り分けの機構は
`scatter.*` に既にあり、足りていないのは「**1 本のソースが自分で持っている
インスタンスを、別々のピースとして扱う**」という入口だけだった。

## 問題

### 1. 配り分けはワイヤの本数でしか起きない

`scatter.*` の `instance_source` は**可変長入力グループ**で、2 本以上繋ぐと
配り分けが働く（`crates/ravel-nodes/src/scatter/mod.rs` の
`attach_instance_sources`）:

```rust
sources => {                                  // ← 2 本以上
    geometry.set_instance_sources(…);
    let source_indices = (0..geometry.instance_count()).map(|index| {
        if random { (hash(source_seed, index as u32) as usize % source_count) as i32 }
        else      { (index % source_count) as i32 }
    }).collect();
    geometry.instances_mut().insert(names::SOURCE_INDEX, …)?;
}
```

`source_mode`（`sequential` / `random`）と `source_seed` もパラメータとして
既にある。**1 本だけのときは別の腕**に落ちる:

```rust
[source] => { geometry.set_instance_source(Some(instance_source(source, center_input)?)); }
```

テキストは 1 本のワイヤなのでこちらに落ち、各点がテキスト全体をスタンプする。

### 2. テキストは既に「1 文字 = 1 インスタンス」を出している

`text.layout`（`crates/ravel-core/src/text/layout.rs`、`TYPE-2`）の出力:

| ドメイン | 中身 |
|---|---|
| Instance（N = 文字数） | `P`（レイアウト位置）/ `rot` / `scale` / `char_index` / `word_index` / `line_index` / `char_progress` / `advance` / `source_index` |
| `sources()`（K = 異なるグリフ数） | グリフ輪郭。**重複排除される**（`by_glyphs` で "aa" は 1 本を共有） |

つまり両ノードは **`source_index` + `sources()` という同じ契約**で話している
（`crates/ravel-core/src/geometry/names.rs` の予約名、`geometry/ops.rs` が扱う）。
テキスト固有の仕組みではない。

### 3. 配れても変調できない

`scatter.*` の出力インスタンスが持つのは自分の `index` / `P` / `rot` / `scale` /
`source_index` だけ。ソース側の `char_index` / `char_progress` は**出力に写らない**。

`per-instance-modulation-plan.md`（`MOD-*`、実装済み）の stagger が読むのは
まさにその属性なので、**グリフを配れてもスタガーが掛けられない**。配り分けだけ
入れて属性を落とすと「1 文字ごとに配れるのに 1 文字ごとに動かせない」状態になり、
これは要件 `REQ-MOGRAPH-004` の眼目を外す。

## 目標アーキテクチャ

### ピースは「インスタンスごと」（決定）

分解の単位は**ソースのインスタンス 1 つ = 1 ピース**。`sources()` の要素
（異なるグリフ）ではない。

`sources()` は重複排除されているので、そちらを配ると **"aa" が 1 ピースに潰れ、
文字順も消える**。インスタンス順は文字順なので、既存の `index % source_count` が
そのままテキストを頭から回る。

### ソースのレイアウト位置は捨て、向きと大きさは焼き込む（決定）

- **`P` は捨てる。** 配置を決めるのは `scatter.*` のパターンで、それがこの機能の
  目的そのもの
- **`rot` / `scale` は焼き込む。** 回った文字は回ったまま配られる方が忠実。
  配置の式は `InstanceTransform`（`TYPE-5` で `rasterize` からコアへ集約した
  もの）に既にあるので、**平行移動だけ外した形**で使う。式を 2 つ持たない

### 明示のモードにする（決定）

```text
piece_mode: "whole"（既定 = 今の挙動） | "instances"
```

暗黙にすると `scatter.*` → `scatter.*` を繋いでいる既存グラフの意味が黙って
変わる。既定は今の挙動のままにして、既存プロジェクトを 1 つも動かさない。

### ピースの属性を出力インスタンスへ写す（決定）

ピースが持っていた Instance ドメインの属性を、そのピースを参照する出力
インスタンスへコピーする。

**名前が衝突するものは写さない。** 出力側が自分で決める
`index` / `P` / `rot` / `scale` / `source_index` は scatter のもので、ソースの
値で上書きしてはいけない（`P` は決定 2 で捨てるものであり、`source_index` は
分解後のピース番号を指す別の値）。写るのはそれ以外 —
`char_index` / `word_index` / `line_index` / `char_progress` / `advance`、および
ユーザーが `attribute.*` で付けた任意の列。

写し方は「ピース i の値を、`source_index == i` の出力インスタンス全部に配る」。
`scatter.grid(500)` に 5 文字を配ると各文字が 100 個ずつ出るので、
`char_index` は 0..4 が 100 回ずつ現れる。**stagger はそれで効く**
（`MOD-*` の既知の制限「順序は `index` の生成順に縛られる」はそのまま）。

### 入れ子の扱い

ピース自身がインスタンスを持つことがある（`scatter.*` の出力を
`"instances"` で分解した場合）。`MAX_INSTANCE_DEPTH`（= 4、
`geometry/container.rs:328`）が描ける深さと展開できる深さを既に揃えているので、
**その上限に従う**。独自の上限を作らない。

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| PIECE-1 | インスタンスをピースへ分解するコアの操作（`ravel-core`、ノード非依存） | — |
| PIECE-2 | `scatter.*` のモードと既存の配り分け経路への接続 | PIECE-1 |
| PIECE-3 | ピースの属性を出力インスタンスへ写す | PIECE-2 |
| PIECE-4 | ロケール / 文書 | PIECE-1〜3 |

### 単位 1: ピースへの分解（コア）

- `ravel_core::geometry::ops` に「インスタンスジオメトリを、インスタンス 1 つ
  = 1 ジオメトリのリストへ分解する」操作を足す。`expand_instances`
  （`ops.rs:1873`、任意のインスタンスジオメトリを 1 枚に平らにする）の**兄弟**で、
  平らにするのではなく**割る**
- 各ピースは `sources()[source_index]` の中身に、そのインスタンスの
  `rot` / `scale` を `InstanceTransform` から平行移動を外した形で焼き込んだもの
- インスタンスを持たないジオメトリは「1 ピース = 自分自身」を返す（素通し）。
  `expand_instances` が冪等であるのと同じ性格
- `source_index` が範囲外・`sources()` が空・Instance ドメインに `P` が無い
  といった欠けは、**`expand_instances` と同じ答え**にする（勝手に原点を
  発明しない）

**完了条件**

- 5 文字のテキストが 5 ピースになり、**同じ文字が 2 回出ても 2 ピース**になる
  （重複排除された `sources()` の数ではない）
- ピースの順序が文字順（Instance ドメインの順）である
- 各ピースの輪郭が、そのインスタンスの `rot` / `scale` を反映し、
  **`P` を反映しない**
- インスタンスを持たないジオメトリが 1 ピースとして素通しする
- 入れ子は `MAX_INSTANCE_DEPTH` に従い、超えた分は `expand_instances` と
  同じ扱いになる

### 単位 2: `scatter.*` のモード

- `scatter.*` の各テンプレート（実装時に `registry/builtin.rs` で数を確認する
  こと）に `piece_mode` を足し、`with_param_options` で
  `["whole", "instances"]` を宣言する。既定は `"whole"`
- **`"instances"` のときは繋がっているワイヤの本数を問わず、全ワイヤを
  それぞれ分解して 1 本のピース列に平らにする**（ワイヤ 2 本 × 文字 5 =
  10 ピース）。ワイヤの順 → その中のインスタンスの順で並べる。
  「ピースを配る」という 1 つの説明で済み、選んだのに効かない控制が
  生まれない（UX 不変条件 6）
- `"whole"` のときは `attach_instance_sources` が今のまま —
  1 本なら丸ごと 1 ソース、2 本以上なら 1 ワイヤ = 1 ソース。
  **既存プロジェクトはこちらに落ちる**
- 分解した結果は**既存の複数ソースの腕へ合流させる**。`source_mode` /
  `source_seed` の意味は変えない
- `center_input` はピースごとに効く（`instance_source` がピース単位で呼ばれる）

**完了条件**

- 既定（`"whole"`）でテキストを繋いだ結果が**今と 1 ピクセルも変わらない**
- `"instances"` でテキストを繋ぐと、点ごとに別の文字が立つ
- 点の数が文字数より多いとき `sequential` は文字列を繰り返す、
  `random` は `source_seed` で決まる（既存の規則のまま）
- **`"whole"` では 2 本以上のワイヤの挙動も今と変わらない**（1 ワイヤ = 1 ソース）
- `"instances"` で 2 本のテキストを繋ぐと、**両方の文字が 1 本のピース列**に
  なる（ワイヤ順 → インスタンス順）
- インスタンスを持たないソースを `"instances"` で繋いでも壊れない（1 ピース）
- **`piece_mode` は `source_mode` と別の軸**であることのテスト:
  `"instances"` × `random` で、ピース列に対して既存の `hash(seed, index)` が
  効く

### 単位 3: ピースの属性を出力へ

- ピースが持っていた Instance 属性を、`source_index` の対応で出力インスタンスへ配る
- **`piece_mode = "instances"` のときだけ。** `"whole"` では「ソース全体」が
  1 ピースなので、その Instance 属性のどの行を配るのか決まらない
- **`index` / `P` / `rot` / `scale` / `source_index` は上書きしない**
- 型と列長の整合は `AttributeSet` の既存の規則に任せる（独自の検証を書かない）

**完了条件**

- `"instances"` で配ったあと、出力インスタンスに `char_index` /
  `char_progress` が乗っている
- `field.attribute(char_progress)` → `field.apply` のスタガーが、配った後にも
  効くゴールデンテスト（**これが要件の眼目**）
- scatter が決める `P` / `rot` / `scale` / `index` / `source_index` が
  ソースの値で壊れていないこと
- 既定（`"whole"`）では属性が 1 つも増えない

## 範囲外

- **`text.layout` 側の変更。** 出力形は `TYPE-2` のままで足りる。この計画は
  受け取り側だけを変える
- **3D。** `instance_source` の中心寄せが平面限定で、`Vec3` ソースは明示エラー。
  ピース分解も同じ線を引く（`orient` / `scale3` が来たら別途）
- **GPU 経路の最適化。** ピースが増えるとソース数が増えるので、
  `gpu-resident-geometry-plan.md` の対象になりうるが、まずは CPU で正しさを出す
- **パス沿いのテキスト（`TYPE-4`）との組み合わせ**の作り込み。`text.path` の
  出力もインスタンスジオメトリなので機構としては通るが、受入条件には含めない

## 決定（2026-09-17）

1. **パラメータ名は `piece_mode`**、値は `whole` / `instances`。既存のモード
   列挙の綴り（`field.time` の `mode`、`text` の `writing_mode`）に合わせた。
   `source_mode` と似た名前が 2 つ並ぶのは受け入れる — 前者は「ピースを
   どう作るか」、後者は「作ったピースをどの順で配るか」で、**別の軸**である
   ことを doc comment とロケールの説明文で言い切る
2. **`"instances"` は繋がっている全ワイヤを分解して 1 本のピース列に平らにする。**
   モードが 1 本のときだけ効く形（選んだのに効かない控制が生まれる）と、
   2 本以上をエラーにする形は却下した
3. **属性を写すのは `"instances"` のときだけ。** `"whole"` ではどの行を配るのか
   決まらない

残った小さい判断（実装の内側なので実装時に決めてよい）: ピースを分解する
コア関数の名前（`expand_instances` の兄弟として何と呼ぶか）。
