# パス操作ノード 実装計画

> **Status**: Planned — 2026-07-27 — **Phase 0 の依存評価で構成が変わる**
> （2026-07-29 に Phase 0 を 0a / 0b に分割。2026-08-06 に `PATH-0b` を決着
> — **三角形分割器は `earcut` クレートを採用（依存追加はユーザー承認済み）**。
> `PATH-0a` は未決）

対象: ブーリアン（パスファインダー）、オフセット、角丸、単純化、トリム。
関連要件: REQ-CORE-010、REQ-MOGRAPH-001、REQ-MOGRAPH-005。

**前提**: `evaluation-scope-plan.md` の group 規約（各 op が `group` を取る）。

## 問題

ベクター編集の基本操作が無い。

| 操作 | 現状 |
|---|---|
| ブーリアン（合流 / 差分 / 交差 / 排他） | **無し** |
| パスオフセット / インセット | **無し** |
| 角丸 | **無し** |
| パス単純化 | **無し** |
| トリムパス | `resample` + `blast` で合成可能だが第一級ノードが無い |

とくにブーリアンは、シェイプ生成（`shape.*`）とタイポグラフィ
（`text.to_path`）の出力を組み合わせる手段が無いことを意味する。

## Phase 0: 幾何クレートの採用判断（作る前に決める）

**2 つの独立した判断に分ける。** 三角形分割器は回避できないがブーリアンは
見送れるので、1 つの単位にすると「ブーリアンを却下したら破砕も作れない」と
いう誤った依存が生まれる。

| ID | 判断 | 見送れるか |
|---|---|---|
| `PATH-0a` | ブーリアンの実装方針（A / B / C） | **見送れる**（C が選択肢） |
| `PATH-0b` | 三角形分割器（クレート / 自前） | **見送れない**（押し出しと破砕が必要とする） |

`geometry-fracture-plan.md` の `FRAC-1` は **`PATH-0b` にのみ依存する**。

### Phase 0a: ブーリアンの実装方針評価

ツリーには `lyon` 1.0.19 と `kurbo` 0.11.3 が既にある（gpui 経由）が、
**どちらもこのバージョンでロバストなパスブーリアンを提供しない**
（lyon は tessellation とパスアルゴリズム、kurbo は曲線数学と交点計算）。

#### 評価する選択肢

**A. 専用クレートを追加**（`i_overlay` / `path-bool` 等）

評価基準:
- ライセンスが Apache-2.0 / MIT 系であること（GPL は
  `.agents/rules/rust.md` により不可）
- **ベジェを保持するか、ポリラインに落とすか**
- 退化ケース（共線エッジ、頂点上の交点、自己交差、ゼロ長セグメント）の
  テストを通すか
- 決定性（同一入力で同一出力）
- 保守状況

**B. ポリライン近似で自前実装**

`flatten.rs` が既にあり、CPU / GPU 両経路が同じ平坦化ポリラインを
消費している（`FLATTEN_TOLERANCE = 0.25px`）。そこに Greiner-Hormann か
Vatti を載せる。新規依存ゼロだが**ベジェが失われ**、退化ケースの処理を
自前で背負う。

**C. ブーリアンを見送り、他のパス操作だけ先行**

#### クレート調査結果（2026-07-29）

**評価基準「ベジェを保持するか」が候補を 2 群に分ける。** これが判断の主軸。

##### ポリラインに落とす群（ベジェが失われる）

| クレート | 実装 | ライセンス | 備考 |
|---|---|---|---|
| [i_overlay](https://github.com/iShape-Rust/iOverlay) | 純 Rust | MIT | union / intersection / difference / xor + 自己交差。i16/i32/i64 と f32/f64 の両 API |
| [clipper2-rust](https://crates.io/crates/clipper2-rust) | 純 Rust | 要確認 | Clipper2 の完全移植。C++ 版と出力を検証済み |
| [clipper2](https://lib.rs/crates/clipper2) | **FFI（C++）** | 要確認 | 内部 i64 で堅牢性を担保。**C++ ツールチェーンを持ち込む**ので不利（現状 C/C++ 依存は FFmpeg のみ） |
| [geo-booleanop](https://github.com/21re/rust-geo-booleanop) | 純 Rust | 要確認 | Martinez-Rueda |

##### ベジェを保持する群

| クレート | 実装 | 備考 |
|---|---|---|
| [path-bool](https://huggingface.co/spaces/openfree/graphite2/blob/main/libraries/path-bool/README.md) | 純 Rust | PathBool.js の移植（Graphite 由来）。複数サブパス・自己交差・fill rule に対応し、線 / 2次 / 3次ベジェ / 楕円弧を扱う。自己交差する3次ベジェを先に単純化し、全エッジ間の交点をグラフ化する方式 |
| [linesweeper](https://github.com/jneem/linesweeper) | 純 Rust | ベジェパスで囲まれた集合に対する robust な Bentley-Ottmann |

[kurbo の boolean は tracking issue 段階](https://github.com/linebender/kurbo/issues/277)で、
**曲線同士の交点が9次多項式に帰着する**という本質的な難しさが議論されている。
ツリーにある kurbo 0.11.3 では使えない、という Phase 0 冒頭の前提は変わらない。

**含意**: 「B（自前実装）はベジェが失われるが A なら保持できる」という単純な
対比ではない。**A のうちポリライン群を選んだ場合、B と同じくベジェは失われる**。
ベジェ保持を要件とするなら候補は `path-bool` / `linesweeper` に絞られる。

`path-bool` は `Fracture` という演算を持つが、これは**交点で全エッジを分割する**
という boolean 用語の fracture で、Voronoi 破砕（`geometry-fracture-plan.md`）とは
別物。名前が衝突するので混同しないこと。

##### ライセンスの確認が未了

`.agents/rules/rust.md` は GPL を禁じている。上表の「要確認」は本評価で必ず
確定させる。`path-bool` は Graphite 由来なので、**元プロジェクトのライセンスを
辿る必要がある**。

### Phase 0b: 三角形分割器の採用判断

三角形分割器は**回避できない**（ブーリアンと違って「見送る」選択肢が無い）。
したがってこの判断は `PATH-0a` の結論と無関係に決着させる。

- `geometry-fracture-plan.md` の既定経路（三角形分割 → 半平面クリップ）が使う
- 3D の押し出しキャップ（前面 / 背面の穴あき多角形を埋める）が使う
  （`3d-scene-plan.md`）

候補:

| クレート | 実装 | 備考 |
|---|---|---|
| [earcut](https://crates.io/crates/earcut) | 純 Rust | Mapbox earcut 移植。内部バッファと出力インデックス列を再利用してアロケーションを避ける |
| [earcutr](https://github.com/donbright/earcutr) | 純 Rust, unsafe なし | 同上。Vec 上に双方向循環リストを実装 |
| 自前実装 | — | earcut 相当で 300 行程度。穴はブリッジ挿入で対応 |

毎フレーム評価される経路なので、**アロケーションを再利用できる `earcut` が有利**。

#### 調査結果（2026-08-06）

`earcutr` の参照先は [donbright/earcutr](https://github.com/donbright/earcutr) から
[frewsxcv/earcutr](https://github.com/frewsxcv/earcutr) へ移っており、
**後者は archived で README に非推奨通知がある**（誘導先が `earcut`）。

| 軸 | `earcut` 0.4.11 | `earcutr` 0.5.0 | 自前実装 |
|---|---|---|---|
| ライセンス | `MIT OR Apache-2.0`（mapbox 由来部分は ISC）。**可** | `ISC`（パーミッシブ）。**可** | 該当なし |
| 最新版と保守 | 2026-07-26 リリース。GeoRust org 管理。上流 earcut 3.2.3 に追随（2026-07-25）。open issue 3 件はいずれも欠陥報告ではない（Miri 導入 / `unsafe` 除去検討 / geo-types 対応） | 2025-05-29 リリース。**リポジトリは archived で、README が `earcut` へ誘導する**（crates.io 上は yank も deprecate もされていない）。open issue 2 件は 2023 / 2024 起票のまま | 自分たちが背負う |
| 依存グラフ | `num-traits ^0.2` のみ。`num-traits 0.2.19` は既に `Cargo.lock` にあるので**増える crate は `earcut` 1 個**。`no_std` + `alloc` | `num-traits ^0.2` + `itertools ^0.14` | 0 |
| API 形 | `Earcut::<f32>::new()` を保持し `earcut<N: Index>(data: impl IntoIterator<Item = [T; 2]>, hole_indices: &[N], triangles_out: &mut Vec<N>)`。`u32` が `Index` を実装するので**出力を `Vec<u32>` に直接書ける** — `Geometry::indices`（`Arc<Vec<u32>>`）と `push_mesh(verts, &[u32])` の形にそのまま合う。入力は反復子なので `Vec2` を `[f32; 2]` に写すだけで中間 `Vec` が要らない | `earcut(vertices: &[T], hole_indices: &[usize], dims: usize) -> Result<Vec<usize>, Error>`。フラット `x0,y0,x1,y1,…` 入力なので中間バッファが要る | 望む形に作れる |
| バッファ再利用 | **可**。内部バッファ（`data` / `nodes` / `queue` / `sort_queue` / `sort_scratch` / `hole_blocks`）は `clear()` されるだけで解放されない。`Earcut` インスタンスと出力 `Vec<u32>` を評価側で持ち回れば毎フレームのアロケーションが消える | **不可**。呼び出しごとに `Vec<usize>` を新規確保し、内部連結リストも都度構築する。加えて `usize` → `u32` の変換パスが乗る | 可 |
| 決定性 | `HashMap` / `HashSet` を使わない（`no_std`）。穴の整列は安定ソートで、`partial_cmp` が `None` のときは `Equal` に落として入力順を保つ。z-order は座標から計算する決定的ハッシュ。`Refiner`（任意の Delaunay リファイン）のハッシュ表も乱数シードを持たない自前オープンアドレス法。**同一入力・同一バイナリで同一出力** | `HashMap` 不使用。同じ mapbox アルゴリズムなので決定的 | 実装次第（`HashMap` の反復順に依存しないこと） |
| 退化入力 | 頂点 2 個以下は空出力で早期 return。リング構築が失敗しても早期 return。フィクスチャ 60 本（`degenerate` / `empty_square` / `hourglass` / `self_touching` / `self_tangent_1..4` / `touching_holes*` / `infinite_loop_jhl` ほか）と**上流 issue 番号付きの回帰 16 本**、乱択プロパティテストを持つ。**panic するのは 2 つだけで doc に明記**: `hole_indices` が単調非減少でない / 頂点数を超える場合と、内部ノード領域が `u32` バイトオフセットを超えた場合（頂点 2^31 個相当、実質到達しない） | 入力検証は `Result<_, Error>` だがバリアントは `Unknown` 1 つで診断にならない。内部に `unwrap()` が数箇所ある | FRAC-1 の完了条件として全部自分で書く |
| `unsafe` | 4 箇所（`node_at` / `node_at_mut` の生ポインタ演算と、それを呼ぶマクロ 2 種）。安全契約が doc コメントにあり `debug_assert!` で範囲とアラインメントを検査する。上流で除去 issue が進行中 | **0**（README も「unsafe 無しで `Vec` 上に連結リストを実装」と明言） | 0 にできる |
| f32 / f64 | `T: num_traits::float::Float` で generic。`Vec2(pub f32, pub f32)` / `Vec3` は f32 なので **`Earcut<f32>` をそのまま使え、座標変換が要らない**。`FLATTEN_TOLERANCE: f32 = 0.25`（`crates/ravel-nodes/src/flatten.rs`）が作る平坦化ポリラインも f32。精度が要る入力向けに整数座標 + 厳密判定の `earcut::int::EarcutI32` が別途ある | 同じく `T: Float` generic | 選べる |

補助 API が 2 つ効く。`deviation()` は多角形面積と三角形分割面積の差を返すので
**FRAC-1 の「全三角形の面積の和が元の面積と一致する」テストにそのまま使える**。
`utils3d::project3d_to_2d` は同一平面上の 3D 多角形を 2D へ射影するので、
`3d-scene-plan.md` 単位 8 の押し出しキャップに効く。

**決定: `earcut` 0.4.11 を採用（production 依存の追加はユーザー承認済み、2026-08-06）**

根拠:

- `earcutr` はリポジトリが archived で、README 自身が `earcut` を指す。候補として脱落する
- 依存の増分が 1 crate。`num-traits` は既にツリーにあり、C / C++ ツールチェーンを持ち込まない
- バッファ再利用が API に組み込まれている。FRAC-1 の「毎フレーム評価されるので
  バッファを再利用する」を、呼び出し側の工夫ではなく型で満たす
- 出力が `Vec<u32>` に直接入るので `Primitive::Mesh` の索引表現と一致する
- f32 generic なので座標の詰め替えが起きない
- 退化ケースの回帰資産（フィクスチャ 60 本 + 上流 issue 回帰 16 本）は自前実装で
  再現できない。本計画の判断基準「モーショングラフィックスでは退化ケースを必ず踏む」に直結する

対抗案を採らない理由:

- **`earcutr`**: 保守が止まっていることに加え、出力 `Vec` を再利用できず `usize` → `u32` の
  変換が毎フレーム乗る。唯一の優位である `unsafe` 0 は、`earcut` 側の `unsafe` が
  2 関数・`debug_assert` 付き・上流で除去作業中であることと釣り合わない
- **自前実装**: **300 行という見積りは FRAC-1 の完了条件と噛み合わない**。
  `earcut` の中核（Delaunay リファインを除く）は空行とコメントを除いて 1177 行、
  `earcutr` は 1088 行ある。z-order 加速と Delaunay リファインを捨てても、
  耳刈り + 穴のブリッジ挿入 + 自己交差の局所修復 + 分割フォールバックで 300 行には
  収まらない。さらに FRAC-1 は「退化入力でパニックしない」を完了条件にしているが、
  上流 earcut が issue 番号付きの回帰を 16 本抱えている事実は、その条件が初回実装で
  満たせる性質のものでないことを示す。z-order を省くと大きな入力で O(n^2) になり、
  毎フレーム評価では効いてくる

`FRAC-1` への申し送り: `hole_indices` の単調性は呼び出し側で検証し、破れていたら
`earcut` に panic させずに `GeometryError` へ落とすこと（`earcut` は doc 記載どおり
ここで panic する）。

確認に使った URL（すべて 2026-08-06 確認）:

- <https://crates.io/api/v1/crates/earcut>（版・公開日・ライセンス・依存）
- <https://github.com/georust/earcut>（README・`src/lib.rs`・`tests/`・issue・コミット履歴）
- <https://docs.rs/earcut/latest/earcut/>（公開 API）
- <https://crates.io/api/v1/crates/earcutr>（版・公開日・ライセンス・依存）
- <https://github.com/frewsxcv/earcutr>（README の非推奨通知・`src/lib.rs`・archived 状態）

### 判断基準

ブーリアンは「だいたい動く」が通用しない領域。モーショングラフィックスでは
形が毎フレーム変わるため、**退化ケースを必ず踏む**。静止画なら手で直せるが
アニメーションでは直せない。

`PATH-0a` の判断:

- **A の候補が基準を満たす** → A で単位 1 を実施
- **満たさない、または依存追加が承認されない** → C に落とし、単位 2 以降のみ
- B は「A も C も選べない場合」の最後の手段とする。ベジェ喪失は
  タイポグラフィとの組み合わせで実用上かなり痛い

**Voronoi 破砕は `PATH-0a` の結論を待たない。** Voronoi セルは凸なので、
三角形分割 + 半平面クリップで厳密に実装できる（`geometry-fracture-plan.md`）。
ブーリアンは同ノードの**任意選択のアルゴリズム**として後から足せる。
ただし `PATH-0b`（三角形分割器）には依存する。

**完了条件**: `PATH-0a` は候補クレートの評価結果（ライセンス / ベジェ /
退化ケース / 決定性）を本ファイルに追記し、A / C のどちらかを Status に記録する。
`PATH-0b` は採用する三角形分割器を記録する。**production 依存の追加は
ユーザー承認を得る**（`.agents/rules/rust.md`）。

## 実装単位

### 単位 1: `path.boolean`（`PATH-0a` が A の場合のみ）

- `mode`: `union` / `difference` / `intersection` / `exclusion`。
- 入力は可変グループ。3 つ以上は左から順に畳む。
- 属性の扱いを明示する: 結果の点は元の点と 1 対 1 対応しないため、
  **点属性は最近傍から転送**し、プリミティブ属性は第 1 入力から取る。
  Detail は第 1 入力。

**完了条件**

- 2 つの矩形の union / difference / intersection / exclusion の
  点列検証テスト。
- **退化ケース**: 完全一致する 2 図形、辺を共有する 2 図形、
  接するだけの 2 図形、内包関係、非交差。
- 自己交差パスを入力にしたテスト。
- 穴あき結果（difference で中央がくり抜かれる）が
  non-zero winding で正しく描かれるゴールデンテスト。
- 同一入力の 2 回評価が一致するテスト。

### 単位 2: `path.offset`

- `distance`（負でインセット）/ `join`（miter / round / bevel）/
  `miter_limit`。
- 自己交差の除去が要る（オフセット距離が曲率半径を超えると
  ループが生じる）。除去しない場合は**その旨をパラメータで明示**し、
  黙って壊れた形を返さない。

**完了条件**

- 矩形のオフセットが期待寸法になるテスト。
- 負のオフセットで内側に入るテスト。
- 曲率半径を超えるインセットでの挙動が定義どおりであるテスト。
- 各 join モードのゴールデンテスト。

### 単位 3: `path.round_corners`

- `radius` / `group`。角度が閾値以下の角のみ丸める `max_angle`。
- 隣接セグメントが短すぎる場合は半径を自動縮小する。

**完了条件**

- 矩形の角が指定半径の円弧になるテスト。
- 半径がセグメント長を超える場合の自動縮小テスト。
- 既に滑らかな点が変化しないテスト。

### 単位 4: `path.simplify`

- Ramer-Douglas-Peucker。`tolerance`。
- ベジェ制御点（`in_tan` / `out_tan`）を持つ点は**既定で保持**する
  （曲線を勝手に潰さない）。`fit_curves` オプションで曲線近似も行う。

**完了条件**

- 直線上の冗長点が除去されるテスト。
- `tolerance = 0` で入力と一致するテスト。
- 端点が保持されるテスト。
- 接線付き点が既定で保持されるテスト。

### 単位 5: `path.trim`

`resample` + `blast` の合成でも書けるが、第一級ノードにする。
線画アニメーション（描かれていく線）で最も使う操作で、合成で毎回組ませない。

- `start` / `end`（0..1 の弧長比率）/ `offset` / `per_primitive`。
- `per_primitive` オフでジオメトリ全体の弧長を通して扱う。

**完了条件**

- `start = 0, end = 1` で入力と一致するテスト。
- 中間区間の弧長が期待値になるテスト。
- `offset` によるループ（end < start で巻き戻る）のテスト。
- 閉パスの扱いのテスト。
- 属性が弧長で補間されるテスト。

### 単位 6: レジストリ / ロケール / 文書

## 検証

- 全てヘッドレス。ゴールデンは CPU ラスタライズ経路。
- **退化ケースのテーブル駆動テストが本計画の中心**。とくに単位 1。
- 決定性: 全ノードで同一入力の 2 回評価一致を確認する。

## 非対象

- **メッシュのブーリアン**。パスのみ。
- **パスの自己交差解消**（単体の操作として）。オフセット内部の処理としてのみ。
- **可変幅オフセット**（テーパー）。
- **パスの整列 / 分布**。`geometry-ops-plan.md`。
