# CLI から書き出す（`ravel-cli`）

> 索引: [`README.md`](README.md)

GUI を起動せずに `.ravprj` をレンダリングする経路。要件は
[`../requirements/REQ-RENDER.md`](../requirements/REQ-RENDER.md) の
REQ-RENDER-005、設計の根拠は
[`../implementation/render-export-plan.md`](../implementation/render-export-plan.md)。
型と関数の地図は [`../agent-api-reference.md`](../agent-api-reference.md) の
`ravel-cli` 節。**このページは「どう使うか」だけを書く。**

## 出荷物は 2 本ある

| バイナリ | クレート | 中身 |
|---|---|---|
| `ravel` | `crates/ravel-app` | GPUI アプリケーション |
| `ravel-cli` | `crates/ravel-cli` | ヘッドレスな書き出し。`gpui` / `ravel-ui` / `ravel-dock` / `ravel-app` に依存しない |

`.ravprj` の読み書きは `crates/ravel-project`（GUI 非依存）にある。CLI は
そこを通してプロジェクトを**読むだけ**で、ロード時マイグレーションの結果も
書き戻さない。GUI が同じファイルを開いていても安全で、ロックを要らない。

## ビルド

```bash
cargo build -p ravel-cli                    # 連番書き出しはこれで足りる
cargo build -p ravel-cli --features ffmpeg  # 素材のデコードが要るとき
```

- **`--workspace` でビルドしない。** Cargo が 1 ビルド内でフィーチャを統合する
  ので `ravel-app` の `ravel-audio/playback` が波及し、CoreAudio / ALSA が
  リンクされる。理由は [`../../AGENTS.md`](../../AGENTS.md) の Repository map。
- **`ffmpeg` は既定 OFF。** 連番書き出しは FFmpeg を要らないが、**音声素材と
  動画素材のデコードには要る**。フィーチャ無しのビルドは音声を出さず、
  `audio-not-rendered` の警告を 1 度出して映像だけを書く（黙って無音の WAV を
  置いたりはしない）。

## `ravel-cli render`

```bash
ravel-cli render project.ravprj -o out/ --range 100-199 --format png
```

| 引数 | 既定 | 意味 |
|---|---|---|
| `<PROJECT>` | — | 読む `.ravprj`。**書き換えない** |
| `-o` / `--output <DIR>` | 必須 | 連番を書くディレクトリ。無ければ作る |
| `--comp <NAME_OR_ID>` | ルートコンプ | 名前でも数値 ID でも指定できる |
| `--range <START-END>` | コンプ全体 | **両端 inclusive** の絶対フレーム番号。`42` だけなら 1 フレーム |
| `--format <FMT>` | `png` | `png` / `exr` / `vp9` / `av1` / `prores` / `h264` / `h265` |
| `--png-depth <8\|16>` | `8` | PNG のチャンネルあたりビット数 |
| `--prefix <TEXT>` | `frame_` | フレーム番号の前に付く文字列 |
| `--suffix <TEXT>` | （空） | フレーム番号と拡張子の間に付く文字列 |
| `--padding <N>` | `4` | フレーム番号のゼロ詰め桁数（下限。超えたら伸びる） |
| `--param <NAME=VALUE>` | — | 公開パラメータの差し替え。繰り返せる |
| `--overwrite` | 拒否 | 既存ファイルへの上書きを許す |
| `--no-audio` | 音声あり | 映像だけを書く |
| `--progress <MODE>` | `auto` | `auto` / `bar` / `json` / `quiet` |

**書けるのは連番だけ。** `--format` は動画コンテナも受け取るが、コンテナの
書き手がまだ無いので `codec-no-writer`（終了コード 5）で開始前に落ちる。
`ravel-cli list codecs` の `writable` がその区別を持っている。

### 出力ファイル名

```text
<prefix><フレーム番号:0 詰め><suffix>.<拡張子>    → out/frame_0100.png
<prefix><先頭>-<末尾><suffix>.wav                → out/frame_0100-0199.wav
```

**フレーム番号は `--range` に依らず絶対値**。`--range 100-199` の 1 枚目は
`frame_0000.png` ではなく `frame_0100.png` になる。

`--prefix` / `--suffix` に区切り文字・`..`・ドライブ記号は入れられない
（出力ディレクトリの外へ出る名前を拒否する。判定は書き出し側のホスト OS に
依らず全プラットフォーム共通）。

### 範囲を分割して複数プロセスで回す

ファイル名が絶対フレーム番号なので、**同じ出力ディレクトリへ互いに素な範囲を
書く複数プロセス**がそのまま成立する。レンダーファームは外から組む。

```bash
ravel-cli render p.ravprj -o out/ --range 0-99   &
ravel-cli render p.ravprj -o out/ --range 100-199 &
wait
```

- 上書き拒否は**ファイル名単位**なので、上のような分割は衝突しない
- 音声も範囲ごとの WAV になり、**サンプル列を連結すると 1 回通したものと
  一致する**（RIFF ヘッダが 2 つ並ぶのでバイト連結ではない）
- 分割できるのは連番だけ。プロセス内並列レンダリングと、分割・投入・結合の
  制御層は非対象

## 音声

音声レイヤーを持つコンポは、フレームと同じ範囲の WAV が連番の**横に**出る。
連番に音を入れる場所が無いための併置で、動画コンテナへの多重化は書き手が
できてからになる。

- 形式は **48kHz ステレオ 32bit float 固定**。問い合わせる装置が無いので
  フラグも無い。別レートの素材は変換して入る
- WAV は一時名で書き、**フレームが揃ってから本来の名前へ rename** する。
  途中で失敗・中断しても、本来の名前には何も残らない
- 音声を出さない状況は必ず言う: `--no-audio` を付けた（`audio-not-rendered`）、
  ビルドに `ffmpeg` が無い（同じ ID・別の文面）、素材がオフラインまたは
  デコード上限超過（`audio-source-skipped`）。**どれも警告で、失敗にはしない**

## `--param` — 公開パラメータの差し替え

差し替えの対象は REQ-PROJ-006 の**公開パラメータ宣言**（`--param <名前>=<値>`）。
レイヤー名やノードパスではないので、プロジェクトの内部構造を変えても外から見た
契約が壊れない。宣言の作り方は
[`../specifications/ui/properties.md`](../specifications/ui/properties.md) 側。

```bash
ravel-cli render p.ravprj -o out/ \
  --param title="Hello" --param tint=1,0,0,1 --param logo=assets/new.png
```

- ベクタと色は**カンマ区切り**の成分
- 値は**宣言された型に対して**解釈する。宣言に無い名前・型不一致・不在の素材は
  **1 フレームも評価する前に**終了コード 4 で落ちる
- どんな宣言があるかは `ravel-cli list params`

## 列挙（機械可読）

```bash
ravel-cli list comps  project.ravprj   # コンプ: id / 名前 / 解像度 / fps / 尺 / ルートか
ravel-cli list params project.ravprj   # 公開パラメータ宣言
ravel-cli list codecs                  # このビルド・この機械で書ける出力
```

いずれも整形済み JSON を stdout に出す。**対話モードはこの 3 つを呼んで
選択肢を作る**ので、専用の列挙経路を増やさないこと。

`list codecs` は**使えない行も理由付きで出す**（省略しない）。

| フィールド | 意味 |
|---|---|
| `format` | `--format` に渡す綴り |
| `kind` | `image-sequence` / `video` |
| `available` | この機械で経路があるか |
| `route` | `native` / `ffmpeg:<encoder>` / `platform:<api>:<encoder>` |
| `reason` | 使えない理由の安定 ID（`ffmpeg-not-linked` 等） |
| `writable` | **Ravel 側に書き手があるか。** `available: true` と `writable: false` の組は機械ではなく Ravel の欠落 |

## `ravel-cli interactive`

```bash
ravel-cli interactive project.ravprj
```

コンプ・形式・出力先・公開パラメータを順に聞いてから書き出す。引数を覚えて
いなくても使えるようにするための層で、**非対話経路にできないことは何もできない**
（答えは `render` の引数そのものへ組み立てられ、同じ検証を通る）。

- **標準入力が端末でなければ入らない。** 明示指定なら理由付きで失敗する
  （`not-interactive`、終了コード 2）。パイプの先で入力待ちに入る事故を
  構造的に防ぐ
- 進捗表示は常にプログレスバー。この経路から JSON は出ないので、バーが
  機械可読出力を横切ることがない

## 進捗と結果

`--progress auto` は **stdout が端末かどうか**で決まる。端末ならバー、
リダイレクトされていれば JSON。バーは常に stderr に出るので、stdout を
パースしている側に混ざらない。

`--progress json` は 1 行 1 オブジェクトで stdout に出す。

| `event` | 主なフィールド |
|---|---|
| `note` | `id`（警告の安定 ID）、`message` |
| `progress` | `job`、`frame`、`rendered`、`total_frames` |
| `completed` | `frames`、`directory`、`first`、`last`、`audio` |
| `failed` | `error`（安定 ID）、`exit_code`、`message`、`detail` |

**`message` は翻訳される文で、`id` / `error` は翻訳されない安定 ID。**
スクリプトが見るのは後者。

## 終了コード

| コード | 意味 |
|---|---|
| 0 | 成功 |
| 1 | 内部失敗（GPU アダプタが取れない、割り込みハンドラを入れられない） |
| 2 | 引数不正。`clap` のパース失敗、未知・曖昧なコンプ名、空の範囲、端末の無い `interactive` |
| 3 | プロジェクトを読めない |
| 4 | `--param` が宣言に無い / 型が合わない / 素材が不在 |
| 5 | 指定した形式をこのビルド・この機械で書けない（書き手が無い場合も含む） |
| 6 | 出力先に既存ファイルがある（`--overwrite` 無し） |
| 7 | 評価に失敗 |
| 8 | エンコードに失敗 |
| 9 | 中断された |

9 は 130 ではない。Windows に `128 + signal` の慣習が無く、コードは全
プラットフォームで同じ意味でなければならないため。

## 何が「開始前」に落ちるか

**2〜6 はフレームを 1 枚も評価する前に決まる。** 引数と文書だけで決まる判定
（形式・コンプ・範囲・パラメータ・出力名）は純関数に寄せてあり、出力の衝突検査は
GPU コンテキストを作る前に走る。したがって**アダプタの無い機械でも**、
`--param` の誤りは 4、既存の出力は 6 として返る（全部 1 に潰れない）。

中断（Ctrl-C）はフレーム境界で効き、書きかけの出力を消す。割り込みハンドラを
入れられなかった場合はレンダリングを始めずに 1 で落ちる（「中断すると何も
残らない」がハンドラ無しでは成り立たないため）。

## 関連

- 何が今日実際に動くか: [`../ui-impl-status.md`](../ui-impl-status.md)
- GUI 側の書き出し（ダイアログとレンダーキューパネル）の設計意図:
  [`../specifications/ui/render-queue.md`](../specifications/ui/render-queue.md)
- 型と関数: [`../agent-api-reference.md`](../agent-api-reference.md)
- 検証の書き方: [testing.md](testing.md)
