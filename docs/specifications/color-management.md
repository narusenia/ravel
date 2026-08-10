# カラーマネジメント仕様

Ravel が画素の値を**どの色空間で持ち、どこで変換するか**の規範。

対応要件: REQ-CORE-009（32bit float リニア空間での処理）、
REQ-RENDER-003（OCIO + GPU LUT カラーマネジメント）。
実装計画は [color-management-plan.md](../implementation/color-management-plan.md)。

この文書はフェーズ CM の `CM-1`〜`CM-5`（自前の固定変換による骨格）までを
記述する。`.ocio` 設定の読み込みと GPU LUT（`CM-6`〜`CM-8`）は**変換の中身を
差し替えるだけで、以下の構造は変えない**。

## 原則

1. **作業空間はリニア Rec.709 原色。** 合成・ブレンド・不透明度・ブラー・
   ダウンサンプルはすべてリニア光の上で行う。
2. **色空間は 2 軸で表す。** 原色（`Primaries`）と伝達関数（`Transfer`）を
   別々に持ち、1 つの列挙に混ぜない。「符号化済みか線形か」が型に出ない
   モデルは、取り込みと表示のどちらでも判断できない。
3. **実行時の画素変換の点は 3 つだけ。** 入力・表示・出力。それ以外の場所で
   伝達関数を適用しない。**例外は永続化の移行**（v7 → v8 の
   `Document::linearize_colors`）で、これは画素ではなく**保存された値の
   読み替え**を一度だけ行うもの。版印がその一度きりを守る。
4. **アルファは変換しない。** アルファは被覆率であって光ではない。
5. **量子化の規則は 1 つ。** 最近接（`* max + 0.5`）。切り捨ては 1.0 を
   最大コードへ写せず、往復が壊れる。

## 構成

```text
素材（sRGB PNG / Rec.709 動画 / リニア EXR …）
   │  入力変換（素材ごとの入力色空間 → 作業空間）
   ▼
作業空間: linear Rec.709 primaries, 32bit float
   │  合成・ブラー・不透明度・ブレンドはここで行う
   ├──── 表示変換 ──▶ Viewer（作業空間 → 表示色空間 = sRGB）
   └──── 出力変換 ──▶ Encoder の手前（PNG / 動画 → sRGB、EXR → 作業空間のまま）
```

## 型

`ravel_core::color`。副作用も I/O も持たない。

| 型 | 意味 |
| --- | --- |
| `Primaries` | `Rec709` / `Rec2020` / `ApOne`（ACEScg の AP1） |
| `Transfer` | `Linear` / `Srgb` / `Rec709` / `Pq` |
| `ColorSpace` | 上の 2 つの組。`SRGB` / `LINEAR_REC709` / `REC709` / `ACES_CG` / `REC2020_PQ` に名前がある |
| `ColorSpace::WORKING` | 作業空間（= `LINEAR_REC709`） |
| `ColorSpace::DISPLAY` | 表示・8bit 出力の色空間（当面 `SRGB` 固定。選択は `CM-8`） |
| `CubeLut` | `.cube` 形式の 3D LUT（読み込みと三重線形補間。**ファイルは読まない** — 呼び出し側がテキストを渡す） |

### 伝達関数の規約

- **定義域外で panic しない。** 負値は奇関数として拡張（`-f(-v)`）、1 超は
  そのまま外挿する。32bit float の合成は日常的に範囲外の値を持つ。
- **往復誤差は 1e-6 以内**（`0..=1` では絶対値、それを超える範囲では相対値）。
- **区分関数の継ぎ目の跳びは有界**。sRGB の `0.04045` と `0.0031308` は
  丸めた値で厳密には交わらないので、規格どおりの実装でも微小な段差が出る。
  Ravel は**符号化側の閾値を線形側から導出**して往復を厳密にし、段差を
  1 箇所（1e-7 未満）に閉じ込める。Rec.709 は規格の `1.099` が粗すぎて
  往復条件を満たさないため、**分岐が厳密に交わるよう係数を導出**している。
- **PQ は `0..=1` の外で直線外挿する。** ST 2084 は `N ≈ 1.992` に極を持ち、
  素朴な逆変換は無限大を返す。

## 入力変換

素材の入力色空間は `MediaAssetEntry` ごとに決まる。**全部 sRGB と決め打た
ない** — リニアな EXR に伝達関数を二重適用してしまう。

解決の優先順位（`MediaAssetEntry::input_color_space()`。永続化される
フィールドは `MediaAssetEntry::color_space` で、下の 1 がそれを読む）:

1. **明示指定** — `MediaAssetEntry::color_space`。ユーザーが設定したもので、
   常に勝つ。設定 UI は `CM-8`
2. **ファイルのメタデータ** — `AssetMetadata::color_space`
3. **拡張子ごとの既定** — float 形式（`exr` / `hdr`）→ リニア Rec.709、
   整数形式 → sRGB

2 と 3 は**推定**なので、どちらを採ったかを素材ごとに 1 度ログへ出す。
TIFF / DPX は float も log も入りうるが**リニア扱いにしない**: 表示参照の
ファイルをリニアと誤ると二重に明るくなり、その逆より被害が大きい。

変換は `ravel-media` のデコード経路、画素の正規化の**直後**
（`ravel_core::color` の `ingest_rgba8` / `ingest_rgba16` /
`ingest_rgbaf32` — 素材のビット深度に合うもの）。`ravel-media` の入口は既定で
無変換（`ColorSpace::WORKING`）なので、ファイルそのものの値が欲しい経路は
その既定のまま読める。メディアビンのサムネイルは**解決済みの入力色空間を
渡して**作業空間で読み、他の 8bit 出口と同じ表示変換
（`to_display_rgba8`）を掛けてから PNG へ量子化する — sRGB 素材では
その往復が恒等なので見え方は変わらず、リニア素材（EXR / HDR）の
サムネイルは display 空間で生成される。

### 取り込みのビット深度

デコードは**素材の画素形式から**取り込み経路を選ぶ（拡張子ではない）。

- float の RGB 形式（EXR などがデコードされる `GBRPF32` 系）はスケーラを
  通さず面を直接読む。**1 超の値はクリップしない** — HDR の要点なので
- 8bit を超える整数形式（ProRes 422 10bit、DNxHR、16bit 静止画）は
  RGBA64（16bit）へスケールしてから `ingest_rgba16` で取り込む
- 8bit 素材は従来どおり 8bit RGBA 経路で、出力はビット単位で変わらない

メタデータ（優先順位 2）はコンテナの宣言をプローブが読む。
`VideoStreamInfo` が `color_primaries` / `color_trc` を Ravel の語彙
（`Primaries` / `Transfer`）へ写し、**名前のある組だけ**が
`AssetMetadata::color_space` に載る。宣言が無い、または Ravel が名前を
持たない組（Rec.2020 原色 + BT.709 OETF など）は `None` のまま拡張子既定へ
落ちる。

## 表示変換

`ravel_nodes::DisplayTransform`（`display_transform.wgsl`）**1 箇所**。
評価ワーカーの `finalize` が、フレームが GPU から降りる**前**に 1 パス
掛ける（`CM-7`）。降りてくるのは 1 画素 4 バイトの表示バイト列
（`DisplayFrame`、BGRA）で、`ravel-app` はそれを包むだけ — CPU 側に
画素ごとの色計算は残っていない。

- 表示色空間は `ColorSpace::DISPLAY`（sRGB 固定）
- `quality` / `ViewerResolution` と**直交**する。あちらは「どの画素を評価
  するか」、こちらは「画素の値が何を意味するか」
- 人間が読む色の表示はすべて表示変換を通る — カラーピッカー、16 進表示、
  数値読み出し。**作業空間の生値を人に見せない**
- **表示変換を持つのは Viewer のワーカーだけ。** 書き出しのワーカーと
  `ravel-cli` は `GpuEvalHooks::with_display_transform` を呼ばず、リニアの
  フレームを受け取って `to_output_space` で自分で符号化する。書き出しが
  Viewer の表示 LUT を継ぐことはない

`ravel_core::color::to_display_rgba8` は**定義**として残る。シェーダはその
第 2 実装で、カラースウォッチのような画素ループでない経路は今も CPU 側の
関数を呼ぶ。`scripts/lint-patterns.sh` の `raw-pixel-quantisation` が、自前の
`* 255.0` / `* 65535.0` を機械的に禁じている（`.wgsl` は走査対象外なので、
シェーダ側の一致はリントではなくテストが担保する）。

### GPU と CPU の一致基準: 1 コード以内

**ビット単位の一致は要求しない。** `to_display_rgba8` は伝達関数を `f64` で
評価するが WGSL には `f32` しかなく、`pow` の精度も規格上「許容誤差付き」で
ドライバに委ねられている。したがって基準は

> **チャネルあたり 8bit コードで ±1 以内。**

コード境界から誤差以内にある値はどちらへ丸んでもよく、256 段中の 1 段は
表示の識別限界より細かい。これより広く外れたらシェーダは別の変換を計算して
いる、と読む。`crates/ravel-nodes/tests/display_transform.rs` の
`the_gpu_and_cpu_roads_agree_within_one_code` が境界を跨ぐ 516 画素で
これを固定する（Metal / Apple Silicon の実測では差 0 コード。**0 を規範に
しない**のは、他のドライバでそれを約束できないから）。

`NaN` はこの基準の外。CPU 側の境界表は `NaN` を 0 に落とすが、これは
`partition_point` の副産物であって WGSL に同じ保証は無い。

### ユーザー提供の `.cube`

`GpuEvalHooks::set_display_lut` で 3D LUT を差し込める。**LUT は伝達関数を
置き換える**: 入力は作業空間のリニア値、出力は表示符号化済みの値。外すと
既定（`ColorSpace::DISPLAY` の伝達関数）に戻る。

- 補間は `CubeLut::sample` と同じ三重線形。テーブルはファイル順のまま
  1 枚の 2D テクスチャ（1 行 4096 テクセルの畳み込み）に載せ、**LUT の
  再アップロードは差し替え時のみ**でフレームごとには起きない
- `CM-9` が OCIO の表示変換を 3D LUT に焼くときも同じ口に入る

**UI からは届かない。** `.cube` を選ぶ設定 UI は `CM-8` の担当で、今は
API とテストだけがこの経路を通る。

### 変換できなかったときは黙らない

シェーダがコンパイルできない、デバイスを失った — 表示変換が走らなかった
とき、`GpuEvalHooks::finalize` は **`None` を返す**。`None` は
「この値は配信するがキャッシュには入れない」という契約なので、変換されて
いないリニアのフレームがホストへ届き、**失敗が焼き付かない**（次の要求で
やり直す）。**CPU で救済はしない**（変換点を二重化しないため）。
届いたリニアのフレームは**ホストがエラーとして出す**:
`ViewerUpdate::from_eval` はリニアのフレームを
`viewer.display_transform_failed` のエラーオーバーレイに変える。リニア光を
そのまま描くのは誤り、黙って黒画面にするのは不親切、という判断。
フレームでない出力（`Scalar` 等）は従来どおり空白。

### 完了条件の検証には GPU アダプタが要る

`CM-7` の完了条件はほぼ全部が GPU テスト
（`crates/ravel-nodes/tests/display_transform.rs`）で、アダプタが無い環境では
既存の GPU テストと同じ作法で skip する。**アダプタの無い CI では、この単位は
実質検証されない。**

アダプタ無しでも走るのは次の 2 つだけ:

- `display_transform.wgsl` の naga による検証と MSL / HLSL / SPIR-V への変換
  （`gpu_util::shader_translation` が全ビルトインシェーダを走査する）
- LUT のアトラス化（`display.rs` の純関数の単体テスト）

**伝達関数の一致・LUT の補間・`quality` 直交は実機でしか確かめられない。**
この単位に触る変更は、アダプタのある機体で
`cargo test -p ravel-nodes --test display_transform` を通すこと。

### 残っている上限: リードバックそのもの

表示変換が GPU に載っても、フレームは依然として一度 CPU へ降りて UI
ツールキットへ上がり直す。`CM-7` が削ったのは画素ごとの CPU 計算と
リードバック量（1 画素 16 バイト → 4 バイト）であって、往復そのものでは
ない。ゼロコピー表示は `HIGH-09` の残りで、引受先の計画はまだ無い。

**書き出し側は GPU の表示変換を通らない**（出力変換は f32 のまま符号化して
から量子化する 2 段で、8bit 専用の 1 段変換が使えない）。数字と測定条件は
[perf-baseline.md](../implementation/perf-baseline.md)。

## 出力変換

`Encoder` トレイトの**手前**に段を置く（`ravel_core::media::encode::to_output_space`）。
`Encoder` も連番実装も色を知らない。

| 出力 | 色空間 |
| --- | --- |
| PNG 連番（8 / 16bit） | `ColorSpace::DISPLAY`（sRGB） |
| 動画コンテナ | `ColorSpace::DISPLAY`（sRGB） |
| EXR 連番 | `ColorSpace::WORKING`（リニアのまま。無変換・無コピー） |

どの変換かは**出力仕様**（`RenderOutput::color_space`）が決める。エンコーダの
実装が決めるのではない。

### 出口の等価性

4 つの出口（Viewer / PNG / EXR / 動画）は「同じ値」ではなく
**「共通の表示色空間へ変換したあと一致する」**。EXR はリニアのまま書くので
PNG とは数値が違う — 違うことが正しい。

**一致の強さは出口で 2 段ある。** 書き出しの 3 つ（PNG / EXR / 動画）は
`to_output_space` + `quantize_u8` という同じ CPU の道を通るので**ビット単位**
で一致する。Viewer だけは `CM-7` で GPU のシェーダに移ったので、基準は
[1 コード以内](#gpu-と-cpu-の一致基準-1-コード以内)に緩む。

8bit PNG / 動画は同じ `quantize_u8` を通るので素の値でも一致する。
**16bit PNG は `quantize_u16` を通る**ので符号化までは同じでも量子化の段数が
違い、8bit の出口と素の値では一致しない（同じ表示色空間へ戻せば一致する）。

**ただし動画については、一致が保証されるのは「エンコーダに渡す RGBA8 画素」
まで。** その先には RGBA → YUV の変換と非可逆エンコードがあり、ファイルから
デコードし直した画素は PNG と一致しない。Ravel が引き受けるのは
**エンコーダへ渡すまでの色の正しさ**であって、コーデックの往復ではない。

## 永続化

`.ravprj` は **v8** から作者が指定した色をリニアとして保存する。v7 以前の
ファイルは**ロード後の型付きパス**（`Document::linearize_colors`）で一度だけ
`srgb → linear` に読み替える。手順の詳細は
[docs/dev/persistence.md](../dev/persistence.md)。

- **色かどうかはポートの宣言型で決まる**（`COLOR` か `VEC3` / `VEC4` か）。
  値からは判別できない
- **`Constant` は変換、`Keyframes` は各キーを変換**（中間フレームのずれを
  警告）、**`Expression` / `NodeOutput` / `Blend` は変換せず報告**
- グラフの外にある作者指定の色も対象: コンポジションの背景色、
  `exposed_parameters` の `color` 既定値
- **冪等ではない。** `srgb → linear` を二度かければ別の色になる。一度だけに
  するのは**フォーマットのバージョン印**の仕事で、v8 の書庫は二度と変換
  されない

## 非対象（この骨格の範囲外）

- `.ocio` 設定の読み込みと OCIO 由来の GPU 3D LUT（`CM-6` / `CM-9`）
- 表示色空間の選択 UI、`.cube` を選ぶ UI、ACEScg 作業空間（`CM-8`）
- 色順応（chromatic adaptation）。AP1 / Rec.2020 の行列は置いてあるが
  Bradford 変換は入っていない。今日 Ravel が行う変換はすべて Rec.709 → Rec.709
  で行列が恒等になるため問題にならない
- HDR 出力、トーンマッピング、カラーグレーディングノード
