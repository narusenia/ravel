# プロシージャルタイポグラフィ実装計画（REQ-MOGRAPH-004）

> **Status**: Planned — 2026-07-27

対象要件: REQ-MOGRAPH-004（優先度 Must）。関連: REQ-CORE-001、
REQ-CORE-007（統一アニメーションチャネル）、REQ-CORE-010（属性）、
REQ-CORE-012（フィールド）。

**前提**: `per-instance-modulation-plan.md`。文字単位アニメーションを
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
- グリフ 1 個の輪郭は数十〜数百点。文字数を掛けても数千点で、
  `perf-baseline.md` が示す 500 インスタンス 0.007 ms の水準に収まる。
- フォント読み込みとグリフ輪郭は**フレーム不変**なのでキャッシュが効く。
  毎フレーム再計算するのは配置（単位 2 のレイアウト）だけ。

GPU が効くとすれば、文字単位変調（`per-instance-modulation-plan.md`）と
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

- **3D テキスト押し出し / ベベル**。REQ-MOGRAPH-003 の管轄
  （`3d-basics-sketch.md`）。
- **縦中横**、ルビ、割注。
- **JIS X 4051 完全準拠の組版**。禁則は一般的なテーブルによる追い出しのみ。
- **リッチテキスト**（1 レイヤー内での部分的な書式変更）。
  v1 は 1 テキストノード = 1 書式。混植は複数ノード + `geometry.merge`。
- **フォントのプロジェクト同梱**（`.ravprj` へのフォント埋め込み）。
  参照は `AssetPath` で持つが、アーカイブへのコピーは行わない。
- **OpenType バリアブルフォントの軸操作**。
- **絵文字のカラーフォント描画**（`COLR` / `CBDT`）。輪郭のみ。
