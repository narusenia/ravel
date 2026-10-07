# SVG 読み込み 企画書

> **Status**: Proposal（未承認） — 2026-10-07

対象: 外部で描いた SVG を Ravel に持ち込む経路。要件はまだ無い。候補 ID は
`REQ-MEDIA-004`（案）。承認後に要件・backlog・roadmap へ載せる。フェーズ F の後に
着手する前提で書いている。

## 問題

ロゴやアイコンを Illustrator / Figma で描いて動かす、というモーショングラフィックスの
最初の一歩が今の Ravel ではできない。ベクタを作る手段は `shape.*` と
`shape.custom_path`（ペンツール）だけで、外部ファイルからパスを読む経路が無い。
静止画として読むにも `AssetKind::Still` は FFmpeg のデコーダを通るので、SVG は
開けない。

持ち込み方には 2 つの方向がある。

- **A. 編集できるジオメトリとして読む。** パスを `Primitive::Path` に、塗りと線を
  スタイル属性に変換する。下流の `geometry.*` / `field.*` / `style.*` がそのまま効く
- **B. 素材（ラスタ）として読む。** SVG を画像に焼いて、静止画と同じ扱いにする

## 現状の調査結果

### パーサは木にあるが、ヘッドレス側には無い

- `usvg` は **0.48.1** が全プラットフォームで木にある。取り込み元は `gpui-ce`
  （と、その `resvg` 0.48.1）。有効な feature は `text` / `system-fonts` /
  `fontdb` / `harfrust` / `skrifa` / `memmap-fonts` / `writer` ほか
- Windows だけ、`gpui-component` が `resvg = "0.45.1"` を直接持ち込むため
  `usvg` **0.45.1** も木にある
- `ravel-core` / `ravel-nodes` / `ravel-media` / `ravel-cli` の依存グラフには
  どちらの版も無い。`usvg = "0.48"` を直接依存にすると **Cargo.lock に
  パッケージは増えない**が、`cargo build -p ravel-cli` がビルドするクレートは
  増える（`roxmltree` / `svgtypes` / `kurbo` / `tiny-skia-path` など）
- 版は `gpui-ce` フォークが決めている。フォーク側が `usvg` を上げると、Ravel 側の
  指定を追従させない限り 3 つ目の版が木に入る
- 余談: ルート `Cargo.toml` の `rustybuzz` のコメントは「`usvg` の下に既にある」と
  書くが、`rustybuzz` 0.20 を引くのは Windows 専用の `usvg` 0.45.1 だけで、
  macOS / Linux では `ravel-core` 自身が唯一の取り込み元になっている

### ジオメトリ側の受け皿

- パスは `Primitive::Path { verts: Range<usize>, closed }`。ベジェは点属性
  `in_tan` / `out_tan`（点からのオフセット、0 なら角）で持つ。3 次ベジェだけを
  表すので、2 次ベジェは 3 次に厳密変換できる
- 塗りと線は要素ごとの属性: `fill`（Bool）、`Cd` / `alpha`、`stroke_width`、
  `stroke_color`、`stroke_align`、`cap`、`join`、`dash`、`dash_offset`
- 塗り規則は **NonZero のみ**。`rasterize` は同じスタイルの閉パスが続くと 1 つの
  `FillRun` にまとめて NonZero で塗る（文字の穴はこれで開く）。evenodd は無い
- 要素ごとのグラデーション塗りは `PSHADE-6`（未着手）。今あるのは後処理の
  グラデーション（`FX-3`）だけ
- グループに当たるものは整数属性 `piece`（`evaluation-scope-plan.md` の piece
  反復）と Bool 属性による group 規約
- 座標は Y 下向き。`shape.*` は中心 `(0, 0)` を既定にし、detail 属性 `anchor` に
  バウンディングボックスの中心を書く

### 素材の受け皿

- `MediaAssetEntry` は参照だけを持ち、ファイルは埋め込まない。パスは
  絶対 / プロジェクト相対 / 変数付きの 3 形式（`AssetPath`）
- `resolved` が `None` ならオフライン。`media` ノードは透明を返し、`ravel-cli` の
  静的走査が `media-offline` を出す（`WARN-2`）
- `AssetWatch` が素材のディレクトリを `notify` で見張り、上書きされると
  `content_revision` を上げてキャッシュを落とす
- `AssetKind` は `Container` / `Still` / `Sequence` の 3 種。SVG を足すなら 4 種目

## 選択肢

### A. 編集できるジオメトリ

`usvg` で解析すると、CSS・`use`・図形（rect / circle / polygon …）・相対コマンドは
解決済みのパス木になる。各パスを次のように写す。

| SVG | Ravel |
|---|---|
| サブパス 1 本 | `Primitive::Path` 1 本。`Z` で `closed` |
| 直線 / 2 次 / 3 次ベジェ | 点 + `in_tan` / `out_tan`（2 次は 3 次へ昇格） |
| 単色の `fill` / `fill-opacity` | `fill = true`、`Cd`、`alpha` |
| `fill="none"` | `fill = false` |
| `stroke` 一式 | `stroke_width` / `stroke_color` / `cap` / `join` / `dash` / `dash_offset` |
| `transform`（入れ子含む） | 点座標へ焼き込む。線幅は行列の `sqrt(|det|)` 倍 |
| 要素 / グループ | `piece`（要素ごとの通し番号）と文字列属性 `name`（SVG の `id`） |

ジオメトリとして読む方式は、さらに 2 つに分かれる。

- **A1. 読み込みノード。** `svg` 素材を参照するノードが評価のたびに（キャッシュ
  越しに）ジオメトリを出す。ファイルが変われば追従する。点の手編集はできないが、
  下流のノードでいくらでも加工できる
- **A2. 一回きりの変換。** 読み込み時にパスごとに `shape.custom_path` ノードを
  生成する。点を手で直せる。代わりにファイルとの縁が切れ、`custom_path` は 1 ノード
  1 パスなので、パス 500 本の SVG はノード 500 個になる

### B. ラスタ素材

`resvg` 0.48.1 も木にあるので、SVG の機能をほぼ全部（フィルタ・マスク・
グラデーション・テキスト）忠実に描ける。代わりに編集できず、拡大すれば解像度を
決め直して描き直す必要がある（CPU。キャッシュ前提）。Ravel の強みである
プロシージャルな加工が一切効かない。

### 比較

| 観点 | A1 ノード | A2 変換 | B ラスタ |
|---|---|---|---|
| 下流の geometry / field / style | 効く | 効く | 効かない |
| 点の手編集 | できない（後述の焼き込みで可） | できる | できない |
| ファイル変更への追従 | 自動 | しない | 自動 |
| SVG 機能の忠実度 | 部分的 | 部分的 | ほぼ完全 |
| 文書の大きさ | ノード 1 個 | パスの数だけ | ノード 1 個 |
| 新しい依存 | `usvg` の直接エッジ | 同左 | `resvg` の直接エッジも |

## 推奨

**A1 を v1 にする。** B は v1 で扱えない要素が多いファイル向けの後続にし、
A2 は「焼き込み」コマンドとして後から足す（A1 の出力を `custom_path` 群へ書き出す）。

理由は 3 つ。持ち込んだ SVG を動かすという目的に効くのは下流のノード群で、それが
効くのは A だけ。ファイル追従は既存の素材の仕組み（`AssetWatch` と
`content_revision`）がそのまま使えるので、A1 は A2 より作る物が少ない。
A2 の「ノード 500 個」はノードエディタの可読性と undo の粒度を両方壊す。

### v1 の範囲

| 入れる | 入れない（読み込み時に警告を出す） |
|---|---|
| path と基本図形、`use`、CSS（`usvg` が解決する範囲） | フィルタ、`pattern` |
| 単色の塗りと線、不透明度 | グラデーション（v1 は最初の stop の色で塗る。`PSHADE-6` 後に対応） |
| `transform` の焼き込み | `clipPath` / `mask`（`path-ops-plan.md` の boolean で後から） |
| `viewBox` と単位 | `<text>`（下記） |
| グループ → `piece` / `name` | `<image>`、アニメーション（SMIL）、外部参照 |

`fill-rule="evenodd"` は NonZero で塗って警告する。evenodd 用のアイコンは
NonZero と同じ見た目になることが多いが、自己交差するパスでは穴が埋まる。

`<text>` は `usvg` の `text` feature でパスに変換できる。ただしフォントは
読み込んだ機械のシステムフォントで解決されるので、**同じファイルが機械ごとに違う
形になる**。v1 では入れず、使いたい人には SVG 側でアウトライン化してもらう。

### 座標と単位

- SVG のユーザー単位 1 = コンポジションの 1 ピクセル。`usvg` が `mm` / `pt` /
  `%` を解決する
- `viewBox` の中心を原点 `(0, 0)` に置き、`anchor` に書く。`shape.*` の既定と
  揃えるため（左上を原点にする案は未決事項 3）
- Y 下向きは SVG と同じなので反転しない
- 色は sRGB として読み、作業空間（線形）へ変換する

### ファイル参照

**素材にする**（`AssetKind` に `Vector` を足す。名前は案）。パス文字列の
パラメータにすると、オフライン表示・相対パス・`Save As` での付け替え・変更監視・
`media-offline` の静的走査を全部作り直すことになる。

これは `effects-library-plan.md` の `FX-1b`（LUT の `.cube` を「パス文字列か素材か」）と
同じ問いで、**両方を同じ答えにする**のがよい。SVG を素材にするなら、LUT も
「デコードしない素材」の 1 種として同じ `AssetKind` の拡張で受けられる。

### 再読み込み

- ファイルが上書きされたら `content_revision` が上がり、ノードのキャッシュが落ちて
  読み直す。undo 段は積まない（`done/result-provenance-plan.md` と同じ扱い）
- 下流が `piece` の番号で要素を選んでいると、SVG 側で要素を足したときにずれる。
  `name`（SVG の `id`）で選ぶよう利用者向け文書で勧める
- オフラインなら空のジオメトリを返す。`media` と同じく評価全体は落とさない

## 実装単位（案）

ID は承認後に確定する。依存は上から順。

### `SVG-0` 依存の確認

- `usvg = { version = "0.48", default-features = false }` を直接依存にしたときの
  `Cargo.lock` の差分と、3 プラットフォームの `cargo tree` を記録する

**完了条件**

- `Cargo.lock` にパッケージが増えないこと、または増えるものとその理由が文書にある
- `cargo build -p ravel-cli` で増えるクレートの一覧が文書にある

### `SVG-1` `usvg::Tree` → `Geometry` 変換（純粋関数）

- 置き場所は `ravel-media::svg`（案）。GPU にも GPUI にも触れない
- 上の対応表どおりに写し、範囲外の要素は警告のリストとして返す

**完了条件**

- 直線・2 次・3 次・閉じたサブパス・複数サブパスの SVG 文字列から、点・`in_tan` /
  `out_tan`・`closed` を検査するテスト
- 入れ子の `transform` が焼き込まれ、線幅が `sqrt(|det|)` 倍になるテスト
- `fill="none"`、単色、`fill-opacity` × `opacity` の属性値を検査するテスト
- 範囲外の要素（フィルタ・グラデーション・`<text>` ほか）が 1 種につき 1 件の
  警告になるテスト
- **重なる別要素の巻き方向**: 同じスタイルの閉パスが続くと `rasterize` が 1 つの
  `FillRun` にまとめるので、逆巻きで重なる 2 要素が打ち消し合わないことを
  ゴールデンで確かめる。打ち消すなら、run を要素の境目で切る手段をこの単位で決める
- 出力が `Geometry::validate()` を通る

### `SVG-2` 素材の種類と取り込み経路

- `AssetKind::Vector`（案）。`.svg` の拡張子で分類し、FFmpeg の probe を通さない
- `.ravprj` の形式番号を上げ、`docs/dev/persistence.md` の手順で移行を書く

**完了条件**

- メディアビンへのドロップと File > Import で `.svg` が素材になるテスト
- 保存して開き直すと同じ素材に戻る往復テスト。旧形式の文書が読めるテスト

### `SVG-3` 読み込みノード

- ノード名は `import.svg`（案）。`asset_id` パラメータで素材を参照する
- 解析結果を `(asset, content_revision)` でキャッシュする

**完了条件**

- ファイルを上書きすると次の評価で形が変わるテスト
- オフラインで空のジオメトリになり、`ravel-cli render` が `media-offline` を出すテスト
- `asset_id` をワイヤで駆動すると `identifier-not-static` が出るテスト
- `docs/dev/add-node.md` のチェックリスト（ロケール・アイコン・登録）を満たす

### `SVG-4` レイヤーの雛形

- SVG をタイムラインへドロップすると、`import.svg` → `rasterize` のネットワークを
  持つレイヤーができる

**完了条件**

- ドロップでレイヤーができ、Viewer に描かれる。undo 1 回で消える

### `SVG-5` 文書

- 要件 `REQ-MEDIA-004`（案）を起こし、`overview.md` に行を足す
- 利用者向けに v1 の範囲と警告の意味を書く

**完了条件**

- `mise run docs:check` が通る

### v1 の後

- `SVG-6` 焼き込み: `import.svg` の出力を `shape.custom_path` 群へ書き出す（A2）
- `SVG-7` グラデーション: `PSHADE-6` の後
- `SVG-8` クリップ: `path-ops-plan.md` の boolean の後
- `SVG-9` ラスタ素材（B）: `resvg` で描く。忠実度が要るファイル向け

## 非対象

- SVG の書き出し
- SMIL / CSS アニメーションの再生
- PDF / AI / EPS の読み込み
- Lottie（別形式。要望が出てから別の企画書）

## 未決事項（ユーザー判断）

1. **方式**: A1 ノード / A2 一回きりの変換 / B ラスタ / A1 + 後から A2・B。
   推奨は **A1 を v1、A2（焼き込み）と B を後続**
2. **ファイル参照**: 素材（`AssetKind` を拡張）/ パス文字列のパラメータ。
   推奨は **素材**。`FX-1b` の LUT も同じ答えにする
3. **原点**: `viewBox` の中心 / `viewBox` の左上 / ノードのパラメータで選ぶ。
   推奨は **中心**（`shape.*` と揃う）。デザインツールとの位置合わせを重視するなら
   パラメータで選ばせる
4. **`<text>`**: v1 では入れない / システムフォントでパス化して入れる（機械ごとに
   形が変わる）/ 同梱フォントだけで解決する。推奨は **入れない**
5. **グラデーション**: v1 は最初の stop の色 / 平均色 / 読み込みを拒否。
   推奨は **最初の stop の色 + 警告**。`PSHADE-6` 後に正しく塗る
6. **埋め込み**: SVG は小さいので `.ravprj` に埋め込む選択肢もある。
   参照のみ（今の素材と同じ）/ 埋め込みも選べる。推奨は **参照のみ**。
   埋め込みは素材全体の方針として別に決める
7. **`usvg` の版**: `gpui-ce` の版に合わせて追従する / 独自に固定する。
   推奨は **追従**（Cargo.lock を増やさない）。フォーク更新時に Ravel 側の指定も
   上げることを依存宣言のコメントに書く
