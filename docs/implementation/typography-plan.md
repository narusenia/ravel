# プロシージャルタイポグラフィ実装計画（REQ-MOGRAPH-004）

> **Status**: Planned — 2026-07-27

対象要件: REQ-MOGRAPH-004（優先度 Must）。関連: REQ-CORE-001、
REQ-CORE-007（統一アニメーションチャネル）、REQ-CORE-010（属性）、
REQ-CORE-012（フィールド）。

**前提**: `done/per-instance-modulation-plan.md`。文字単位アニメーションを
専用機構ではなくフィールド変調で実現する設計なので、変調層が先に要る。

## 問題

テキストが**まったく存在しない**。`text.*` ノードも、フォント読み込みも、
テキストレイヤーテンプレートも無い。`procedural-geometry.md` の
「既存コードへの影響」表でも Text 展開は 🔶 未のまま。

主要ユースケースがリリックモーション（要件記述）なので、これは
REQ-MOGRAPH で最も欠けている Must。

一方で土台は揃っている。

- フォントスタックは**既に依存ツリーにある**（gpui / gpui_platform 経由で
  swash 0.2.10、cosmic-text 0.19、rustybuzz 0.20、ttf-parser 0.25、
  unicode-bidi、unicode-linebreak）。要件が名指しする swash + zeno も含む。
  新規の production 依存追加は実質不要（ワークスペースへの直接指定のみ）。
- グリフ輪郭 → `Geometry`（Path プリミティブ）への変換先は
  `shape.custom_path` と同じ形で済む。
- 変調はフィールド層（per-instance 変調計画）がそのまま効く。

## 決定事項（2026-07-27 設計セッション）

### テキストは「レイヤー種別」ではなくノード

`LayerSource` の構造的分岐は撤廃済み（`composition/mod.rs:10`）。
テキストは `text.layout` ノードが `Geometry` を出す形にし、
`assets/layer-templates/text.ron` を足して「テキストレイヤーを作る」
操作を既存の LayerTemplate 機構に載せる（`shape.ron` と同型）。

### 文字単位アニメーションはフィールド変調で行う

専用の Animator / Range Selector データモデルは**持たない**。
`text.layout` が per-character 属性を付け、`field.attribute` →
`field.curve_remap` → `field.apply` で変調する。per-instance 変調計画の
資産をそのまま使い、テキスト専用の変調機構を二重に作らない。

付与する属性（Instance ドメイン、1 文字 = 1 インスタンス）:

| 属性 | 型 | 意味 |
|---|---|---|
| `index` | I32 | 文字通し番号（既存の標準属性） |
| `P` / `rot` / `scale` | Vec2 / F32 / Vec2 | 配置（既存の標準属性） |
| `char_index` | I32 | 行内の文字番号 |
| `word_index` | I32 | 単語番号 |
| `line_index` | I32 | 行番号 |
| `char_progress` | F32 | 文字列全体での正規化位置 0..1 |
| `advance` | F32 | 文字送り量（縦書き対応時に方向が変わる） |

`char_progress` を持たせるのは、stagger に必要な「index / 総数」を
毎回 `field.attribute(normalize)` で組ませないため。

### AE 相当は subnet プリセットとして同梱する

「フェードイン + 下から出る + index でずらす」のような定番は、
毎回ノードを 5 個繋がせない。既存の `assets/layer-templates/*.ron` と
同じ RON 形式で**ノードプリセット**（subnet の中身）を同梱し、
ノードエディタから 1 操作で挿入できるようにする。

これで「基盤はノードグラフ、入口は 1 クリック」の両立になる。

### v1 は横書き。縦書きは後半の単位

cosmic-text は縦書き（`writing-mode: vertical-rl`）をまともに扱わない。
`vert` / `vrt2` GSUB フィーチャの適用と縦メトリクス（`vhea` / `vmtx`）の
読み出しを rustybuzz + ttf-parser で自前で組む必要がある。

横書きで「文字がジオメトリになり、フィールドで変調され、パスに沿う」
までを通してから縦に取り掛かる。単位 6 として計画には含める。

### シェイピングは cosmic-text ではなく rustybuzz を直接使う

cosmic-text は「テキストをラスタライズして画面に出す」ためのレイヤーで、
グリフ**輪郭**とクラスタ境界を素直に取り出す API になっていない。
必要なのは shaping（rustybuzz）+ 輪郭（swash / ttf-parser）+ 行分割
（unicode-linebreak）+ 双方向（unicode-bidi）で、cosmic-text の
レイアウト層は挟まない方が制御しやすい。

## 目標構成

```text
text.font ─→ FontRef ─┐
                      ├─→ text.layout ─→ Geometry ─→ field 変調 ─→ rasterize
text.layout params ───┘        │        (per-char instances)
  (string / size / tracking /  │
   leading / align / wrap)     └─→ text.to_path ─→ Geometry (輪郭パス)
                                        │
path geometry ─→ text.on_path ──────────┘
```

`text.layout` は既定で**インスタンス**を出す（1 文字 = 1 インスタンス、
`instance_source` に各グリフの輪郭ジオメトリ）。これで
`scatter.*` と同じ形になり、ラスタライザの既存インスタンス経路
（`rasterize/mod.rs:551`）にそのまま乗る。

`text.to_path` は「文字を粒子にして散らす」「ノイズで歪ませる」用に
インスタンスを**展開**して 1 枚のポイント/パスジオメトリにする。
受入条件「変換ジオメトリがフィールド/パーティクルの影響を受ける」は
こちらが担う。

## GPU 方針

**タイポグラフィは GPU 化の対象外**。理由は構造的なもので、測定待ちではない。

- シェーピング（rustybuzz）とグリフ輪郭抽出（swash）は**本質的に CPU**。
  GPU に持ち込む余地がない。
- フォント読み込みとグリフ輪郭は**フレーム不変**なのでキャッシュが効く。
  毎フレーム再計算するのは配置（単位 2 のレイアウト）だけで、
  文字数分のインスタンス変換にとどまる。

`perf-baseline.md` の 0.007 ms を根拠にはしない。あれは
`scatter.grid(500)` の**キャッシュ温**の計測で、シェーピングや輪郭抽出とは
別種の仕事を測っている。タイポグラフィのコストは**未測定**。

単位 2 の完了条件に、独自のベースライン計測を 1 本入れる:
100 文字 / 1000 文字のレイアウトを未キャッシュで測り、
`perf-baseline.md` に記録する。ここが 16.6 ms を超えるなら
グリフキャッシュの設計を見直す（GPU 化ではなく）。

GPU が効くとすれば、文字単位変調（`done/per-instance-modulation-plan.md`）と
`text.to_path` 後の輪郭点へのフィールド適用（単位 5）で、これは
`gpu-resident-geometry-plan.md` が担う。**本計画は GPU 常駐ジオメトリが
入っても書き換えが要らない形**にする — グリフ輪郭を
`instance_sources` に置き、配置を Instance ドメインの属性で表す設計が
ちょうどそれに当たる（属性列が GPU 常駐になっても構造が変わらない）。

## 実装単位

### 単位 1: フォント解決（`ravel-core` または新 `ravel-text`）

- `FontRef`（`NodeData`）: フォントファミリ名 + ウェイト + スタイル、
  および解決済みのフォントデータ `Arc<Vec<u8>>` と face index。
- システムフォント列挙は `gpui_platform` の `font-kit` フィーチャが
  既に有効なので、それを使う。プロジェクト埋め込みフォントは
  `AssetPath`（`composition/asset.rs`）に載せて相対パス解決に統一。
- `text.font` ノード: ファミリ/ウェイト/スタイル → `FontRef`。
  解決失敗はフォールバックフォント + 警告（評価は落とさない）。

**完了条件**

- 実在するファミリの解決テスト（CI で確実にあるフォントに限る。
  無ければ埋め込みテストフォントを `crates/*/tests/fixtures/` に置く）。
- 未解決ファミリでフォールバックが返り `Err` にならないテスト。
- 同一パラメータで同一 `Arc` が返る（キャッシュ）テスト。

#### 実装メモ

実装済み。**この節が指定した `font-kit` は使っていない。**

- **`font-kit` を採らなかった理由**: 依存ツリーの `font-kit` は
  `zed-font-kit` で、`Cargo.lock` を見ると到達経路は `gpui` と `gpui_macos`
  だけである（ワークスペースの `gpui_platform = { features = ["font-kit"] }`
  はその 2 つのためのもの）。`ravel-core` や `ravel-nodes` から使うには
  GUI クレートへの依存が要り、**`ravel-cli` に CoreText / DirectWrite /
  fontconfig がリンクされる** — `AGENTS.md` が `cargo build -p ravel-cli` で
  守っている性質を壊す。計画時点ではこの到達経路を見ていなかった
- **代わりに `ttf-parser` で `name` / `OS/2` テーブルを読み、プラットフォームの
  フォントディレクトリを走査する**。純 Rust で、`ravel-cli` のリンクは
  変わらない（`otool -L` で確認済み）。新規パッケージの追加はゼロ
  （`ttf-parser 0.25` は `rustybuzz` / `swash` 経由で既に木にあった）
- **捨てたもの**: ファミリのエイリアス解決（`sans-serif` のような総称名、
  ユーザーの `fonts.conf`）。**単位 3（Properties のフォント選択 UI）で
  総称名が要るなら、そこは GUI 側なので `gpui` のフォント一覧を使える** —
  core に font-kit を持ち込む理由にはならない
- **フォールバックは `assets/fonts/Geist-Regular.ttf` を `include_bytes!` で
  埋め込み、通常のフェイスとしても索引に入れた**（実在ファミリの解決テストが
  CI のフォント事情に依存しなくなる）。フォントファイルは追加していないので
  ライセンス表記の位置も変わっていない
- **プロジェクト埋め込みフォント（`AssetPath`）は入れていない** — 評価時に
  プロジェクトルートが届かないので絶対パスしか解決できない半端な
  パラメータになる。UI からファイルを指す経路ができる単位 3 と同時に入れる

### 単位 2: シェイピングとレイアウト → インスタンスジオメトリ

- rustybuzz でシェイプ、unicode-linebreak で折り返し、
  unicode-bidi で双方向を解決。
- `text.layout` ノード: `text` / `size` / `tracking` / `leading` /
  `align`（left/center/right/justify）/ `wrap_width` / `anchor`。
- 出力は per-character インスタンス。上表の属性を全付与。
- 各グリフの輪郭は swash の outline を zeno パスに落とし、
  `Primitive::Path` の集合として `instance_sources` に入れる。
  同一グリフ ID は 1 つの `Arc<Geometry>` を共有し、
  `source_index` で参照する（既存の複数ソース機構をそのまま使う）。

**完了条件**

- ASCII 文字列で文字数 = インスタンス数のテスト。
- 合字・結合文字（例: `ﬁ`、濁点付き仮名）でクラスタが 1 インスタンスに
  なるテスト。**文字数 ≠ コードポイント数**を明示的に検証する。
- 同一グリフが `instance_sources` を共有することのテスト
  （`Arc::ptr_eq`）。
- `align` / `wrap_width` の配置テスト（バウンディングボックス検証）。
- 上表の全属性が付くことのテスト。
- **ベースライン計測**: 100 文字 / 1000 文字のレイアウトを未キャッシュで
  測り `perf-baseline.md` に記録（シェーピング / 輪郭抽出 / 配置を分けて）。

#### 実装メモ

実装済み。`ravel_core::text::layout`（配置ロジック）と
`ravel_nodes::text::LayoutProcessor`（ノード）に分かれている。

- **この節が指定した swash / zeno は使っていない。** `rustybuzz::Face` は
  `ttf_parser::Face` に `Deref` するので、**シェイプしたのと同じフェイスから
  そのまま輪郭が取れる**（`outline_glyph`）。swash を挟む理由が無い。
  `ttf-parser` は単位 1 が既に直接依存にしている
- **依存クレートは 3 本の直接エッジだけで、`Cargo.lock` のパッケージは
  1 つも増えていない**（`rustybuzz` / `unicode-bidi` /
  `unicode-linebreak`。それぞれ `usvg` と `cosmic-text` 経由で既に木にあった）。
  いずれも純 Rust なので `ravel-cli` のリンクは変わらない
  （`otool -L` で確認）。**単位 1 が `font-kit` を採らなかった理由は
  「システムフォント列挙が OS API だった」ことなので、シェイピングのクレートには
  一般化されない**
- **`text.rs` を `text/` に割った**（`git mv text.rs text/font.rs` +
  `text/mod.rs` + `text/layout.rs`）。単位 1 の実装メモが予告していた通りで、
  レイアウトが 750 行あり、フォント選択と共有するものが `FontRef` だけだったため
- **輪郭は曲線のまま持つ。** 2 次ベジェを 3 次に上げ、`in_tan` / `out_tan`
  の点属性として置く（ペンツールのパスと同じ表現）。`rasterize` が既に
  共有の flatten を通すので、拡大しても字形が折れず、単位 5 の
  `text.to_path` が本物の制御点をフィールドに渡せる
- **合成座標は Y 下向き**（`rasterize` の規約）なので、フォント単位の
  Y 上向き座標は輪郭を読む時点で反転している。全輪郭が同時に反転するので
  non-zero winding は変わらず、`o` のカウンタもそのまま抜ける
- **`align` は余白ではなく原点に対して測る**（left は原点から始まり、
  center は原点を挟み、right は原点で終わる）。`anchor` は縦だけ
  （baseline / top / center / bottom）。理由は「文字数が変わってもタイトルが
  ずれない」こと。9 通りのアンカーグリッドは持たない
- **行末の空白と `\n` はインスタンスにならない。** 行中の空白はなる
  （空の輪郭を持つ 1 インスタンス）。「ユーザーが数える文字」と
  「インスタンス」を一致させるための線引き
- **`char_progress` は `index / (総数 - 1)`** なので 0..1 を端まで張る
  （1 文字のときは 0.0）
- **天井 2 つ**（どちらもモジュールに明記）:
  - 双方向の並べ替えは**段落単位で、行単位ではない**。単一方向の段落では
    厳密。混在段落の折り返し行は段落に対して並ぶ
  - 段落は **1 回だけシェイプ**して行を切り出す。改行を跨ぐカーニング対は
    行中の幅を保つ。harfbuzz の `UNSAFE_TO_BREAK` が上げ方
- **CJK のコードポイント単位フォールバックは入れていない。** 未解決ファミリが
  同梱フェイスに落ちる単位 1 の挙動をそのまま使う。フォント選択 UI が入る
  単位 3 と同時のほうが、どこで混植を宣言するかを 1 回で決められる
- **`font` 入力が未接続でもエラーにしない** — 既定ファミリを解決する。
  ノードを置いて文字を打てばすぐ出る
- **ノードアイコンは必須だった。** `assets.rs` の
  `every_node_template_icon_is_embedded` が全テンプレートに固有のアイコンを
  要求するので、カテゴリ既定へのフォールバックはテスト失敗になる
  （`docs/dev/add-node.md` が「壊れない」と書いていたのは stale だったので
  直した）。Lucide の `align-left` を `assets/icons/` に vendoring した
- **ベースライン: 1000 文字 0.28 ms = 16.6 ms の 1.7%。**
  グリフキャッシュは入れていない（計画が挙げた条件に 2 桁足りない）。
  輪郭抽出は文字数ではなく**アルファベットの大きさ**に比例することが
  測って分かった（100 文字と 1000 文字で 0.022 ms のまま）。詳細と
  ハーネスは `perf-baseline.md` の「テキストレイアウト baseline」節

### 単位 3: テキストレイヤーテンプレートと Properties

- `assets/layer-templates/text.ron`（`shape.ron` と同型）。
- Properties にフォント選択 UI。gpui-component の `Combobox` で
  ファミリ検索。
- `assets/locales/{en,ja}.toml` にラベル追加。

**完了条件**

- テンプレートからテキストレイヤーを作ると画面に文字が出る
  （ゴールデンテスト、CPU ラスタライズ経路）。
- Properties でフォントを変えると再評価されるテスト。

### 単位 4: パス沿い配置

- `text.on_path` ノード: パスジオメトリ入力 + テキストインスタンス入力。
  既存の `path_sample`（`geometry/ops.rs:143`、弧長ベース、tangent /
  normal 付き）を文字ごとの累積 advance 位置で呼ぶ。
- `offset` / `spacing` / `align`（パス始点/中央/終点）/
  `flip` パラメータ。

**完了条件**

- 直線パスで通常配置と一致するテスト。
- 円弧パスで `rot` が接線に一致するテスト。
- パス長 < テキスト長のときの挙動（クランプ）のテスト。

### 単位 5: `text.to_path`（ジオメトリ化）とフィールド被変調

- インスタンスを展開して 1 枚のジオメトリにする。
  各インスタンスの `P` / `rot` / `scale` を輪郭点に焼き込み、
  per-character 属性を Point ドメインへ伝播する。
- これで文字の**輪郭点**がフィールドの影響を受ける。

**完了条件**

- 展開後の点数が「各グリフ輪郭点の総和」に一致するテスト。
- per-character 属性が Point ドメインに降りることのテスト。
- `field.noise` → `field.apply(P)` で輪郭が歪むゴールデンテスト。
  **受入条件「変換ジオメトリがフィールドの影響を受ける」の検証。**

#### 実装メモ

実装済み。`text.to_path` ノード（パラメータ無し、入力 1 出力 1）+
`ravel_core::geometry::ops::expand_instances`。

- **展開は `text` 固有ではない。** `expand_instances` は任意のインスタンス
  ジオメトリを平らにする。`text.` 名前空間に置いたのは用途がそれだから
  であって、`scatter.*` の出力もそのまま展開できる。インスタンスを
  持たないジオメトリは素通しなので冪等
- **配置の定義を 1 箇所にまとめた。** `rasterize` が持っていた
  scale → rotate → translate の式を
  `ravel_core::geometry::InstanceTransform` に出し、`rasterize` の
  `Placement` はそれに委譲する形にした。展開した絵とラスタライズした絵が
  一致することは「同じ関数を呼ぶ」で担保する（式を 2 つ持たない）。
  `MAX_INSTANCE_DEPTH` も core に移し、描けない深さと展開できない深さを
  揃えた
- **接線は `apply_vector`**（回転とスケールのみ、平行移動なし）。
  `in_tan` / `out_tan` は点からの差分なので、平行移動を掛けると
  全制御点がインスタンス原点に引き寄せられて字形が壊れる。
  なお **`geometry.transform` は接線を変換していない**（既存の穴。
  回転・スケールで曲線パスの制御点が取り残される）
- **輪郭の順序は契約である。** 自分の要素が先、その後にインスタンス 1 つ
  ずつが連続ブロックとして並ぶ。`rasterize` は**連続する**同スタイルの
  閉パスを 1 回のノンゼロ巻き数で塗るので（#510）、2 文字の輪郭を
  混ぜると counter が別の run に落ちて穴が塗り潰される。
  `crates/ravel-nodes/tests/text_to_path_golden.rs` の
  `a_glyph_counter_stays_a_hole_after_the_conversion` が、**文字ごとに
  色を変えた** `oo` で守っている（1 色だと順序が効かないので守れない）
- **衝突したアトリビュートはソース側が勝つ**（`rasterize` の
  `element_style` と同じ規約）。**天井**: インスタンスの `Cd` / `alpha` は
  ラスタライザでは *乗算* されるのに、ここでは欠けている色を *埋める*
  だけなので、色付きソースへのティントは展開で失われる。グリフ輪郭は色を
  持たないのでテキストには影響しない
- **`index` は展開後に振り直す**（`sort` と同じ規約）。グリフ内の点番号でも
  文字番号でもなく、新しい Point ドメインの生成順が正しい。点から文字を
  たどる手がかりは `char_index` / `char_progress`
- **`P` は 2D のみ**。3D のインスタンス位置は `RequiresPlanarP` で
  エラーにする（黙って xy へ射影しない）。画像インスタンスは輪郭を
  持たないので警告して飛ばす

### 単位 6: 縦書きと禁則処理

- `vert` / `vrt2` GSUB フィーチャの適用（rustybuzz）。
- 縦メトリクス `vhea` / `vmtx` の読み出し（ttf-parser）。
- 縦中横（半角数字の横倒し）は**非対象**。
- 禁則処理: 行頭禁則（`。、）」` 等）と行末禁則（`（「` 等）の
  ぶら下げ／追い出し。JIS X 4051 の完全実装は目指さず、
  一般的な禁則文字テーブルによる追い出しのみ。

**完了条件**

- 縦書きで `advance` が Y 方向になるテスト。
- 括弧・句読点が縦書き字形に置換されることのテスト。
- 行頭に句読点が来ないことのテスト。

### 単位 7: ノードプリセットと文書更新

- 定番アニメーション 4〜5 種を subnet プリセットの RON として同梱
  （フェードイン / 下から出る / タイプライター / ウェーブ / ランダム）。
  いずれも単位 2 の属性 + 既存フィールドノードだけで構成し、
  テキスト専用ノードを増やさない。
- ノードエディタからプリセットを挿入する経路。
- `docs/specifications/procedural-geometry.md`: 「既存コードへの影響」表の
  Text 行を更新。標準属性表に per-character 属性を追加。
- `docs/requirements/REQ-MOGRAPH.md`: 004 の受入条件を更新。

**完了条件**

- 各プリセットを挿入して評価が通るテスト。
- `mise run check` が通る。

## 完了条件（要件レベル）

| REQ-MOGRAPH-004 受入条件 | 単位 |
|---|---|
| テキストレイヤーを作成できる | 3 |
| フォント/サイズ/色/行間等の基本属性を設定できる | 1, 2, 3 |
| 文字単位アニメーション（イン/アウト/ウェーブ/ランダム）が動作する | 7（基盤は 2 + 変調計画） |
| パスに沿ったテキスト配置ができる | 4 |
| テキストをパスジオメトリに変換できる | 5 |
| 変換ジオメトリがフィールド/パーティクルの影響を受ける | 5（パーティクルは `particle-plan.md`） |
| テキストアニメーションをノードグラフで構築できる | 2 + 変調計画 |

## 検証

- ゴールデンテストは CPU ラスタライズ経路のみ（GPU 不要）。
- **フォント依存のゴールデンは避ける**。OS 標準フォントは環境で字形が
  変わるため、埋め込みテストフォント（軽量な OFL フォント 1 本）を
  fixtures に置き、画素比較はそれに対してのみ行う。
- シェイピングの正しさは字形の画素比較ではなく、**クラスタ数・advance
  値・属性値**で検証する。

## 非対象

- **3D テキスト押し出し / ベベル**。REQ-3D-004 の管轄
  （`3d-scene-plan.md`）。
- **縦中横**、ルビ、割注。
- **JIS X 4051 完全準拠の組版**。禁則は一般的なテーブルによる追い出しのみ。
- **リッチテキスト**（1 レイヤー内での部分的な書式変更）。
  v1 は 1 テキストノード = 1 書式。混植は複数ノード + `geometry.merge`。
- **フォントのプロジェクト同梱**（`.ravprj` へのフォント埋め込み）。
  参照は `AssetPath` で持つが、アーカイブへのコピーは行わない。
- **OpenType バリアブルフォントの軸操作**。
- **絵文字のカラーフォント描画**（`COLR` / `CBDT`）。輪郭のみ。
