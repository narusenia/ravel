# OFX ホスト 実装計画（REQ-PLUGIN-001）

> **Status**: Planned — 2026-08-05

対象要件: REQ-PLUGIN-001（OpenFX 統合）。
関連: REQ-PROJ-002（プロセス分離）、REQ-INFRA-009 / REQ-GPU-001（GPU 抽象と
interop）、REQ-INFRA-007、REQ-PLUGIN-002（ネイティブプラグイン、別経路）。

## なぜ今書けるのか

`gpu-backend-plan.md` の `GPUBK-8`（interop 出口、#287）が入り、
`MED-GPU-07`（wgpu の一本化、#292）が済んだ。REQ-PLUGIN-001 が
「`GPUBK-8` の完了後でないと計画を書かない」としていた条件が満たされた。

前提が動いて腐るのを避けるための後置だったので、**この計画は `GPUBK-8` が
実際に何を出せるかを実測した上で書いている**。

## 問題

OFX の GPU Render Suite が要求する API と、Ravel が出せるものが**プラット
フォームによって噛み合わない**。ここが計画全体の形を決めるので最初に置く。

### OFX が定義する GPU API（実測）

`AcademySoftwareFoundation/openfx` の `include/ofxGPURender.h` を読んだ結果、
定義されているのは **4 つだけ**。

| API | 有効化プロパティ | ホストが渡すもの |
|---|---|---|
| OpenGL | `kOfxImageEffectPropOpenGLEnabled` | テクスチャ index / target |
| CUDA | `kOfxImageEffectPropCudaEnabled` | `kOfxImageEffectPropCudaStream` |
| Metal | `kOfxImageEffectPropMetalEnabled` | `kOfxImageEffectPropMetalCommandQueue` |
| OpenCL | `kOfxImageEffectPropOpenCLEnabled` | `…OpenCLCommandQueue` / `…OpenCLImage` |

**D3D12 は無い。** ヘッダ冒頭のコメントがそう言っている:

> It allows hosts and plug-ins to support OpenGL, OpenCL, CUDA, and Metal.
> Additional GPU APIs, such a Vulkan, could use similar techniques.

### Ravel が出せるものとの突き合わせ

| プラットフォーム | Ravel の描画 API | `interop` が出せるもの | OFX 側の受け口 | ゼロコピー |
|---|---|---|---|---|
| macOS | Metal | `id<MTLDevice>` / `id<MTLTexture>` / `id<MTLCommandQueue>` | **Metal** | **成立する** |
| Windows | D3D12 | `ID3D12Device*` / `ID3D12Resource*` / `ID3D12CommandQueue*` | 無い（CUDA / OpenCL / OpenGL のみ） | **成立しない** |
| Linux | Vulkan（`GPUBK-12` 待ち） | 未実装 | 無い（同上） | 同上 |

**macOS だけが素直に繋がる。** `id<MTLCommandQueue>` は `MED-GPU-07` で
wgpu を 29.0.4 に揃えた際に取得可能になった（`wgpu_hal::metal::Queue::as_raw`
が上流で復帰した）ので、Metal に必要な 3 つが揃っている。

**Windows は繋がらない。** `GPUBK-8` が用意した D3D12 ハンドルは OFX が
受け取る先を持たない。したがって `GPUBK-8` の D3D12 interop は
**OFX ではなく REQ-GPU-001（HW デコード）側の資産**という位置づけになる。

### 要件書の記述を 3 箇所訂正する

上の実測により `docs/requirements/REQ-PLUGIN.md` に誤りが見つかった。
`OFX-9` で直す（この計画で勝手に書き換えず、単位として持つ）。**3 箇所**ある。

1. 「初期サポート (Phase B) … **GPU Render Suite (OpenGL/Metal/CUDA)**」 —
   **OpenCL が抜けている**。ヘッダは 4 つ定義している
2. **同じ行が OpenGL を初期サポートに数えている**が、本計画は
   OpenGL を実装しない（D3D12 から橋が架からない。上の優先順位表）。
   **「OpenGL は非対応、該当プラグインは CPU 往復へ縮退」と書き換える**。
   縮退することは `OFX-8` の表示に出す
3. 「REQ-INFRA-009 は Metal / D3D12 を直接触る抽象を作るので、**その抽象が
   OFX が要求する texture / device pointer をそのまま露出できる**」 —
   **D3D12 について偽**。露出はできるが OFX に渡す先が無い。
   `REQ-INFRA-009` 側の同趣旨の記述も併せて直す

## 決定事項

### プロセス分離は選択ではなく要件

`REQ-PROJ-002`（Must）の受入条件が名指ししている。

> - [ ] OFXプラグインが別プロセスで実行される
> - [ ] プラグインプロセスのクラッシュがメインプロセスに影響しない

したがって構成は最初から次の形にする。「まずインプロセスで動かして後で分ける」
はやらない — 分離を後付けすると、ホストの状態と Ravel の状態が同一プロセスを
前提に絡んだ後で引き剥がすことになる。

```text
Ravel (Rust)
   │  IPC（制御: 構造化メッセージ / 画像: 共有メモリ）
   ▼
ravel-ofx-host  (C++ 実行ファイル。CMake でビルド)
   │  dlopen / LoadLibrary
   ▼
*.ofx バンドル（サードパーティ）
```

### ビルドは CMake

C/C++ シムは Rust ワークスペースの外に置き、**CMake でビルドする**
（ユーザー判断、2026-08-05）。`cc` クレートを使わないのは、成果物が
静的ライブラリではなく**独立した実行ファイル**だから。

`Ordo`（narusenia/ordo）を使う案も検討したが、**今回は採らない**。理由は
シム側の依存が実質ゼロ（OFX ヘッダを vendoring するだけ）で、Ordo の強みで
ある依存解決を使わない一方、Ninja + Ordo 自体を macOS / Windows 両方の CI に
入れるコストが乗るため。将来ここが育って依存が増えたら再検討する余地は残す。

### OFX ヘッダは vendoring する

`include/ofx*.h` は BSD ライセンスの数ファイル。サブモジュールにも
パッケージマネージャにもせず、**リポジトリに取り込んでバージョンを固定する**。
取り込み元のコミットを `OFX-1` で記録する。

### 画像はプロセス境界を越えてもコピーしない（macOS）

分離の代償として画像がプロセス間を渡る。素直にやると**この計画自体が
`HIGH-05` / `HIGH-04` で潰したリードバックを再導入する**ので、そこを設計の
中心に置く。

macOS の道筋は `IOSurface`。プロセス間で共有でき、両側で `MTLTexture` として
包める。**ただし Ravel 側にその口が無い** — `GPUBK-8` は
「バックエンド固有ハンドルを*取り出す*」エクスポート方向だけを作り、
インポート方向（`create_texture_from_hal` 相当）は
**消費者が無い段階で形を決めると腐る**として意図的に置かなかった。

**その消費者がこの計画。** `OFX-6a` でインポート方向を `interop` に足す。

> **未検証**: `MTLDevice.newTexture(descriptor:iosurface:plane:)` と
> wgpu の `create_texture_from_hal` を繋いで同一 `IOSurface` を両プロセスから
> 包めることは、`OFX-0` のプロトタイプで**実際に確かめる**。ここが崩れると
> macOS のゼロコピーも崩れるので、コードを書く前に確認する。

### Windows は準 1 級。CUDA を後回しにしない

**Windows は Ravel にとって準 1 級のプラットフォーム**（ユーザー判断、
2026-08-05）。したがって「まず macOS、Windows は CPU 往復のまま様子見」は
**採らない**。`OFX-0` の Windows の問いは「やるかどうか」ではなく
**「どの橋を使うか」**であり、`OFX-7a`（CUDA）は `❓` ではなく計画された単位。

候補は 2 つ:

- **D3D12 → CUDA 外部メモリ**（`cudaExternalMemoryHandleTypeD3D12Resource`
  相当）。NVIDIA 限定だが、プロ映像のワークステーションでの支配率と
  プラグイン側の実装率で選ぶなら本命
- **D3D12 → OpenCL 外部メモリ**（OpenCL 3.0 の external memory 拡張）。
  ベンダ中立だが対応状況がまちまち。CUDA が駄目なときの代替

> **未検証**: 上記 2 つの API がそれぞれ現実に使える形で存在するかは
> **`OFX-0` で確認する**。ここでは「候補」として挙げるにとどめる。

#### 共有可能なテクスチャを確保できるかが両プラットフォーム共通の関門

CUDA に D3D12 のリソースを渡すには、そのリソースが**共有可能として確保されて
いる**必要がある（`D3D12_HEAP_FLAG_SHARED` + `CreateSharedHandle`）。macOS で
`IOSurface` を使うのも同じ性質で、**テクスチャの確保時点で外部共有を宣言して
おく**必要がある。

**wgpu にはそれを指示する口が無い。** `TexturePool` が普通に確保した
テクスチャは、後から共有可能にはできない。したがって両プラットフォームとも
**「外部で確保したバックエンドのテクスチャを Ravel 側で包む」口**が要る —
`GPUBK-8` が置かなかったインポート方向そのもの。

これは Metal と CUDA の**共通の前提**なので、単位を分ける（`OFX-6a`）。
Metal のためだけの作業ではないし、CUDA が Metal の完了を待つ理由も無い。

#### CI で検証できないことを設計の前提にする

**GitHub の `windows-latest` ランナーに GPU は無い。** つまり CUDA 経路は
**CI で一度も実行されない**。`GPUBK-8` の D3D12 interop が
「クロスコンパイルでコンパイル確認のみ、実機未検証」で止まっているのと
同じ制約が、今度は**実行時の正しさ**にかかる。

したがって、次の 3 つを前提に置く。

- CUDA 経路は**コンパイル確認を CI で必ず行う**（実行できなくても、
  型とリンクが壊れたら落ちる形にする）
- **実機確認の手順と結果を PR に書く**ことを `OFX-7a` の完了条件にする
- 出力一致テストは、GPU が無い環境では skip する既存パターン
  （`GpuContext::new_blocking().ok()`）に合わせる

**手元の検証環境**（2026-08-05 時点）: macOS（開発機）と
**Windows + NVIDIA** はある。**Radeon（AMD）は無い。**

これが 2 つの判断に効く。

- **CUDA は実機で検証できる。** `OFX-7a` の完了条件に実機確認を課してよい
- **OpenCL は手元では検証できないが、テスターがいる**（ユーザー、2026-08-05）。
  そこで **Experimental として実装する**（`OFX-7b`）。CUDA の代替ではなく
  追加の経路で、CUDA が成立していることが前提

#### 「Experimental」の定義

この計画で Experimental と呼ぶものは、次の 4 つを満たす。

- **既定で無効。** 設定で明示的に有効化する
- **検証が外部テスター頼み**で、CI にも開発機にも載らない
- **UI 上でそうと分かる**（`OFX-8` の縮退表示に "Experimental" として出る）
- **リリースを止めない。** ここの回帰は出荷判断の根拠にしない

この線を引いておかないと、検証されていない経路が既定で動いて
「AMD でも動くはず」という状態になる。それは縮退を隠すのと同じ。

**CUDA そのものの扱いは独立した要件に切る**（ユーザー判断、2026-08-05）。
CUDA バックエンドは `gpu-backend-plan.md` の「非対象」に明記されており、
本計画で決められる話ではない。`OFX-0` の測定結果をその要件定義の入力にする。

#### 4 つの API に優先順位を付ける

「OFX が定義しているから全部やる」ではなく、**D3D12 から橋を架けられるか**と
**プラグインが実際にどれを実装しているか**の 2 軸で切る。

| API | D3D12 からの橋 | プラグイン側の実装状況 | 判断 |
|---|---|---|---|
| **CUDA** | `cudaExternalMemoryHandleTypeD3D12Resource` 相当。**Windows で唯一まともに見込みがある** | GPU 対応の商用プラグインはまず CUDA を持つ | **本命。`OFX-0` で確認する** |
| **OpenCL** | OpenCL 3.0 の external memory 拡張。ベンダ中立だが**対応状況がまちまち** | CUDA より薄い。AMD / Intel 環境での代替として存在する | **二番手。CUDA が駄目なら見る** |
| **OpenGL** | **事実上無い。** D3D ↔ GL 相互運用（`WGL_NV_DX_interop2`）は D3D9 / D3D11 世代で、D3D12 を対象にしていない | 古い世代のプラグイン。新しいものは CUDA / Metal へ移行済み | **やらない** |
| **Metal** | 不要（macOS は Metal で描いている） | macOS の GPU 対応プラグインは Metal | **`OFX-6b` で実装する** |

**OpenGL を落とす理由**は「古いから」ではなく、**橋が架からないから**。
D3D12 で描いた画像を GL テクスチャに渡す標準経路が無いので、実装しても
`D3D12 → CPU → GL` になる。それは CPU 往復と同じコストで、GL コンテキストの
管理と依存だけが増える。**ゼロコピーにならない GPU 経路は、CPU 経路より悪い。**

**OpenCL を二番手に置く理由**は、ベンダ中立という利点が
「拡張の対応状況がまちまち」で相殺されるから。CUDA が NVIDIA 限定なのは
弱点だが、**プロ映像のワークステーションは NVIDIA が支配的**で、かつ
GPU 対応プラグインは CUDA を実装している率が高い。カバー率で見ると
CUDA 1 本の方が OpenCL 1 本より広い可能性がある。

> **未検証**: 上の「橋」列と「プラグイン側の実装状況」列は、いずれも
> `OFX-0` で確認する。とくに `WGL_NV_DX_interop2` が D3D12 を対象に
> しないことと、OpenCL の external memory 拡張の実際の対応状況は、
> **この判断の根拠そのもの**なので思い込みで進めない。

#### CPU 往復で十分かの目安

`perf-baseline.md`（`GPUBK-6` の節）の実測から概算すると、リードバックは
1080p で約 2.4 ms、4K で約 6.4 ms。往復（読み戻し + 書き戻し）はその倍として:

| 解像度 | 往復 1 回 | 60 fps 予算 16.7 ms に対して |
|---|---|---|
| 1080p | 約 5 ms | **30%**（プラグイン 1 つで） |
| 4K | 約 13 ms | **78%**（1 つで予算をほぼ使い切る） |

つまり **1080p でプラグイン 1 つのスクラブなら CPU 往復で耐える**が、
実時間再生や 4K、あるいは複数プラグインでは成立しない。
`OFX-0` はこの概算を実測に置き換える。

### プラグインは Ravel のノードにどう見えるか

**`type_key = "ofx:<プラグイン識別子>"`。バージョンは含めない**
（ユーザー判断、2026-08-05）。インストールされている版に束縛し、
プラグインを更新しても既存プロジェクトが開けなくなることがない形。

**可搬性はデータモデルが既に解いている。** `Node`（`graph.rs:412`）は
`type_key` / `inputs` / `outputs` / `parameters` をすべて自分で持ち、
`GraphDoc` がそれをそのまま直列化する。したがって、次の 2 つが成り立つ。

- **プラグインが入っていないマシンで開いても、ノードはポート・パラメータ・
  配線を保ったまま残る。** 黙って消えない
- 壊れるのは評価だけで、`EvalError::NoProcessorRegistered`（`eval.rs:109`）
  になる。**この故障モードは新設ではなく既存**

したがって `OFX-3` で足すのは「消えないようにする仕組み」ではなく、
**エラーを具体的にすること**（「プラグイン X が見つからない」と言う）と、
**版差の突き合わせ**:

| 保存されたパラメータ | インストール版に | 扱い |
|---|---|---|
| ある | ある（**型と次元も一致**） | 値を復元する |
| ある | ある（**型か次元が違う**） | **保持したまま警告**。既定値へ潰さない — 名前だけ見て型違いに書き込むと黙って壊れる |
| ある | **無い** | **保持したまま警告**。捨てると再インストールで戻せなくなる |
| 無い | ある | プラグインの既定値 |

**バージョンは `type_key` に入れないが、記録はする。** 作成時のプラグイン版を
`NodeMetadata` に持たせ、開いたときの版と違えば警告に添える。
`type_key` に入れると束縛が切れてプロジェクトが開けなくなるが、
**記録しないと「なぜ結果が変わったのか」が誰にも分からなくなる**。
束縛と記録は別の問題として扱う。

### プラグインの走査はユーザーが選べるようにする

Sapphire 単体で 200〜300 プラグインあり、全部 `describe` すると秒単位になる。
**`describe` 結果はバンドルのパス + mtime をキーにディスクへキャッシュする**
（常に）。その上で**再走査のタイミングを設定で選べる**ようにする
（ユーザー判断、2026-08-05）。

| 設定 | 挙動 |
|---|---|
| `background`（既定） | 起動時はキャッシュだけ読む。実走査は背景で、差分が出たらメニューを更新 |
| `startup` | 起動時に同期走査。確実だが起動が遅くなる |
| `manual` | 明示的に指示したときだけ走査 |

`settings.toml` の `[plugins]` に置き、設定画面から変えられるようにする
（`settings-screen-plan.md` の経路に乗る）。

ホストプロセスは**最初の OFX ノードを置くまで起動しない**。走査は
キャッシュとホストプロセスのどちらでも成立する（`describe` はホストが要るので、
実走査時にだけ一時的に起こす）。

> 将来スプラッシュスクリーンを入れるなら、**キャッシュが無い初回走査**を
> その裏に置ける。上の 3 択はその場合も変えなくてよい。

### プロセスは全プラグインで 1 つ

GPU / CUDA コンテキストはプロセスごとに要り高価なので、プラグインごとに
プロセスを分けると GPU メモリがその分割かれる。`REQ-PROJ-002` が求めるのは
**メインプロセスの保護**だけなので、まず 1 つにする（ユーザー判断、2026-08-05）。

**プロトコルにホスト ID を持たせておく。** 後でプラグインごとに分割しても
Rust 側の API が変わらないようにするため。

### 制御は JSON、画像は共有メモリか GPU ハンドル

制御メッセージ（describe / インスタンス生成 / render 要求 / パラメータ設定）は
**JSON をパイプで流す**（ユーザー判断、2026-08-05）。頻度が低く、
**ログをそのまま読める**のが効く — 行儀の悪いサードパーティプラグインを
相手にするとき、протокол を目で追えることの価値が速度に勝る。

- Rust 側は `serde_json`（既にワークスペース依存）
- C++ 側は `nlohmann/json`（ヘッダ 1 ファイル）を **OFX ヘッダと同じく
  vendoring** する。`OFX-1` の作業に含める
- **メッセージにプロトコル版を持たせ、不一致を検出して拒否する**

画像は JSON を通らない。CPU 経路は共有メモリ、GPU 経路は共有ハンドル
（`IOSurface` / D3D12 の共有ハンドル）で、**JSON に乗るのは ID だけ**。

**画像は Ravel 側が確保する。** GPU 経路で共有可能として確保できるのは
wgpu デバイスを持つ Ravel だけなので（`D3D12_HEAP_FLAG_SHARED` /
`IOSurface`）、CPU 経路も揃えて Ravel が確保する。OFX の
`clipGetImage` にはホストがそれを包んで返す。

### プラグインが自前の UI を開く場合

**OFX には Dialog Suite（`ofxDialog.h`）がある。** プラグインが
`RequestDialog()` を呼ぶと、ホストが `kOfxActionDialog` を返し、
プラグインはその中で自前のウィンドウを開く。仕様が 2 つ条件を課している:

> It runs in the **host's UI thread** … Plugin should **return from this action
> when all Dialog interactions are done**. At that point the host will continue
> again.

つまり **UI スレッドで、閉じるまでブロックする**。

**この suite が存在する理由が、そのまま我々の設計の追い風になる。** ヘッダの
冒頭がこう書いている — 「ホストがフルスクリーンで、OFX を別スレッドで
走らせていると、プラグインが自前のウィンドウを開こうとしたとき衝突が多発する」。
**プロセスを分けているので、プラグインのウィンドウは `ofx-host` のもの**であり、
Ravel の run loop を壊しようがない。ここは分離が効く数少ない場面。

ただし**代償が 1 つある**。

- **`ofx-host` は GUI 対応プロセスでなければならない。** run loop
  （macOS は `NSApplication`、Windows はメッセージポンプ）が要る。
  「起動して即終了するコンソール実行ファイル」では Dialog が開けない。
  **`OFX-1` の骨格をその形にしておく**（後から足すと構造が変わる）
- macOS では自前ウィンドウを出すのに activation policy の扱いが要る
  （そうしないと出ない / 前に来ない）
- ウィンドウの所有者が Ravel と別プロセスなので、**Ravel のウィンドウに
  対してモーダルにならない**。背面に回りうる

**Dialog Suite を使わず、パラメータ変更アクションの中でいきなり自前の
ウィンドウを開くプラグインもいる**（suite の存在理由がそれ）。両方を
想定する — 前者は行儀よく待てばよく、後者はこちらが何もしなくても
`ofx-host` プロセスの中で開く。

### パラメータは双方向に流れる

`kOfxParamTypePushButton` は値ではなく**アクション**で、押すと
`kOfxActionInstanceChanged` がプラグインへ飛ぶ。そしてプラグインは
その中で**他のパラメータの値を書き換えてよい**（`paramSetValue`）。

したがって **IPC は Ravel → プラグインの一方向では足りない。**
プラグイン側から来るパラメータ書き込みを受け取り、

- Ravel のパラメータへ反映し、
- **undo に 1 つの操作として乗せ**（`Document` スナップショットが undo 単位）、
- 再評価を起こす

必要がある。`OFX-2` のプロトコルをこの前提で設計する。**片方向で作ってから
双方向にすると、undo の粒度が後から決まらない。**

### OFX のパラメータ型と Ravel の対応

`kOfxParamType*` は 15 種類。Ravel の `ParameterValue`（`graph.rs:205`）と
突き合わせると、**大半は素直に載り、3 つが穴**。

| OFX | Ravel | 備考 |
|---|---|---|
| Double / Double2D / Double3D | `Channel` / `Channel2` / `Channel3` | アニメート可。素直 |
| RGB / RGBA | `Channel3` / `Channel4` | 素直 |
| Integer | `Int` | |
| Boolean | `Bool` | |
| String | `String` | |
| Parametric（`ofxParametricParam.h`） | `Curve`（`CurveParam`） | Ravel はカーブエディタを持つ（`PARAM-2` 済み）ので載る見込み |
| Custom | `String` | 値はプラグインが解釈する不透明文字列。**Ravel は保存と往復だけ**し、編集はプラグインの UI に任せる |
| Integer2D / Integer3D | — | 整数の 2D / 3D が無い。`Channel2/3`（float）に載せるか分解する |
| Choice / StrChoice | `Int` / `String` + 選択肢 | **選択肢の一覧をどこに持つか**が未定 |
| Group / Page | — | レイアウト構造。Properties パネルに階層とページの概念が要る |
| **PushButton** | — | 値ではない。**新しい機構が要る** |
| **Bytes** | — | 不透明バイナリ。`String` では入らない（UTF-8 でない） |

**穴は `PushButton` / `Bytes` / `Group`・`Page`。** いずれも `OFX-5` で
扱いを決める。`Bytes` は `ParameterValue` に variant を足すことになるが、
**永続化フォーマットの変更**（bincode の positional index、journal の版）に
なるので `docs/dev/persistence.md` の手順に従う。

> **重要**: 対応できない型を**黙って落とさない**。落とすと、プラグインの
> UI で設定した内容がプロジェクトから消える。保存だけはして
> 「Ravel からは編集できない」と示すのが最低ライン。

### 縮退はユーザーに見せる

「黙って遅くなる」のが最悪なので、**どのプラグインが GPU 経路に乗り、
どれが CPU 往復に落ちるか**をホストが起動時に判定してユーザーに見せる。
これを受入条件に含める（`OFX-8`）。

## 目標構成

```text
crates/ravel-ofx/          Rust 側。ホストプロセスの管理と IPC のクライアント
     │                     ProcessorRegistry（PLUG-1）に OFX ノードを登録する
     │
     ├─ プロセス起動・監視・再起動
     ├─ IPC プロトコル（制御 + 共有メモリのハンドル受け渡し）
     └─ OFX パラメータ ⇄ Ravel パラメータの写像

ofx-host/                  C++ 側。Rust ワークスペースの外。CMake
     ├─ CMakeLists.txt
     ├─ vendor/openfx/     取り込んだ OFX ヘッダ（BSD、コミット固定）
     └─ src/               スイート実装、バンドルの走査とロード
```

`ravel-ofx` は `ravel_gpu::interop` を使う数少ないクレートになるので、
**`gpu-interop-escape` lint の許可リストに加える**（現在 `ravel-gpu` /
`ravel-media` / `ravel-ofx` を想定した形になっており、名前は既に予約済み）。

## 実装単位

1 単位 1 PR。`OFX-0` は判断ゲートで、結果が出るまで後続を並べない。

| ID | 単位 | 対象 | 依存 |
|---|---|---|---|
| OFX-0 | **前提の検証と Windows 経路の判断（❓ゲート）** | プロトタイプ | GPUBK-8 ✅, MED-GPU-07 ✅ |
| OFX-1 | `ofx-host` の骨格と CMake ビルド、ヘッダ vendoring | `ofx-host/` | OFX-0 |
| OFX-2 | プロセス管理と IPC 境界（クラッシュ隔離を含む） | `ravel-ofx`, `ofx-host` | OFX-1 |
| OFX-3 | バンドルの走査・ロードと Property / Memory Suite | `ofx-host` | OFX-2 |
| OFX-4 | Image Effect Suite（CPU レンダー） | 両方 | OFX-3 |
| OFX-5 | Parameter Suite と Ravel UI への表示 | 両方 + `ravel-ui` | OFX-4 |
| OFX-6a | **`interop` のインポート方向**（Metal / D3D12 共通の前提） | `ravel-gpu` | OFX-0 |
| OFX-6b | Metal GPU レンダー（macOS） | 両方 | OFX-4, OFX-6a |
| OFX-7a | **CUDA GPU レンダー（Windows）** | 両方 | OFX-4, OFX-6a, OFX-0 の判定 |
| OFX-7b | OpenCL GPU レンダー（**Experimental**） | 両方 | OFX-7a |
| OFX-8 | 未対応 Suite の `kOfxStatErrUnsupported` と縮退の可視化 | 両方 + `ravel-ui` | OFX-5 |
| OFX-9 | 文書更新（REQ-PLUGIN-001 の 3 箇所の訂正を含む） | 要件・仕様・`docs/dev/` | OFX-8 |

### OFX-0 前提の検証と Windows 経路の判断（❓ゲート）

**コードを書く前に、この計画が前提にしている 3 つを実際に確かめる。**
プロトタイプは捨てる前提で、リポジトリには入れない（結果だけ記録する）。

1. **macOS のゼロコピーが成立するか。** 同一 `IOSurface` を Ravel 側で
   `MTLTexture` として作り、別プロセスから同じ `IOSurface` を包んで読めるか。
   wgpu 側は `create_texture_from_hal` 相当が要る
2. **Windows の橋渡しが存在するか。** `D3D12 → CUDA` と `D3D12 → OpenCL` の
   外部メモリ経路が、現実に使える形であるか
3. **CPU 往復のコストがいくらか。** 1080p / 4K で、プロセス境界を越える
   往復 1 回あたりの実測。`perf-baseline.md` の既存のリードバック計測
   （`GPUBK-6` の節）と比較できる形で測る

**完了条件**

- 3 つの結果が `perf-baseline.md` に日付付きで記録される
  （過去の記録は書き換えない）
- **Windows でどの橋を使うかを決める**（CUDA / OpenCL）。Windows は準 1 級
  なので「見送り」は既定の選択肢ではない。**どちらも成立しない**と分かった
  場合に限り、その根拠を本節に書いた上で `OFX-7a` を ❌ にする
- Linux（Vulkan）は `GPUBK-12` が無い以上まだ判定できない。**この計画では
  対象外**とし、`GPUBK-12` の後に別途判断する
- macOS のゼロコピーが成立しない場合、**`OFX-6a` / `OFX-6b` の設計をやり直す**。
  この計画の他の単位は影響を受けない
- CUDA の要件定義に渡す入力（往復コストと橋渡しの可否）が揃う

### OFX-1 `ofx-host` の骨格と CMake ビルド

- `ofx-host/` に CMake プロジェクトを作る。成果物は実行ファイル 1 つ
- OFX ヘッダを `ofx-host/vendor/openfx/` に取り込む。**取り込み元のコミット
  ハッシュを `vendor/openfx/README.md` に記録する**
- `nlohmann/json`（ヘッダ 1 ファイル）を `ofx-host/vendor/json/` に同じ形で
  取り込む。制御プロトコルで使う
- **GUI 対応プロセスとして作る。** run loop（macOS は `NSApplication`、
  Windows はメッセージポンプ）を最初から持たせる — Dialog Suite も、
  自前でウィンドウを開くプラグインも、これが無いと成立しない。
  後から足すとプロセスの構造が変わる
- 起動して即終了するだけのホストで、**macOS と Windows の CI に載せる**

**完了条件**

- `cmake --build` が macOS（clang）と Windows（MSVC）で通る
- CI が両方でホストをビルドする。**Rust 側のビルドとは独立に失敗する**
  （シムが壊れても Ravel 本体の CI 結果が読めること）
- ライセンス表記が `assets/fonts/` と同じ扱いで記録される
  （BSD。配布物に同梱が要る）

### OFX-2 プロセス管理と IPC 境界

- Rust 側からホストを起動・監視し、**落ちても Ravel が生き残る**
- 制御メッセージ（describe / render 要求など）と画像の受け渡しを分ける。
  画像は共有メモリ、制御は構造化メッセージ
- **プラグイン起点のパラメータ書き込み**を受け取る（双方向。上記の決定事項）

#### 障害は「そのノードだけ」では済まない

**プロセスが 1 つなので、1 つのプラグインのクラッシュは同じプロセスに
いる全 OFX インスタンスを巻き込む。** これは 1 プロセス構成を選んだ時点で
受け入れた性質で、`REQ-PROJ-002` が求める「メインプロセスに波及しない」は
満たしている。ただし**故障の単位はノードではなくホスト**なので、そう設計する。

- **ホスト単位の故障状態**を持つ。クラッシュ時は、そのホストにいた
  **全インスタンス**を「失われた」とマークする
- 再起動後は**インスタンスを再構築する**（`describe` → インスタンス生成 →
  保存済みパラメータの再適用）。ノードごとに勝手に復活はしない
- **要求にタイムアウトを設ける。** 行儀の悪いプラグインは落ちるだけでなく
  **固まる**。固まった 1 つが全 OFX ノードを止め続けるのが最悪なので、
  タイムアウトで打ち切り、ホストを kill して再起動する
- **再起動の回数を制限する。** 起動 → 即クラッシュを繰り返すプラグインで
  無限ループにしない。上限に達したらそのプラグインを無効化して
  ユーザーに見せる（`OFX-8` の表示に乗る）

> 将来プラグインごとにプロセスを分けるなら、この「ホスト単位」の粒度が
> そのまま細かくなるだけで、Rust 側の API は変わらない
> （プロトコルにホスト ID を持たせてあるため）。

**完了条件**

- ホストプロセスを外から `kill` しても Ravel が落ちず、
  **そのホストの全 OFX ノードがエラーになり、他は無事**である統合テスト
- 再起動後にインスタンスが再構築され、**パラメータの値が復元される**
- **応答しないホストがタイムアウトで打ち切られる**テスト
  （固まったプラグインを模したスタブで確認する）
- 再起動回数の上限に達したプラグインが無効化され、その旨が表に出る
- **プロトコルのバージョン不一致を検出して拒否する**
  （ホストと Ravel の版ずれは配布事故として必ず起きる）

### OFX-3 バンドルの走査・ロードと Property / Memory Suite

- 標準パス（macOS / Windows）の `*.ofx` バンドルを走査する
- `OfxGetNumberOfPlugins` / `OfxGetPlugin` を呼び、`describe` まで通す
- ホストが提供する側の Property Suite と Memory Suite を実装する

- `describe` 結果をバンドルのパス + mtime をキーにキャッシュする
- 走査タイミングの設定（`background` / `startup` / `manual`）を
  `settings.toml` の `[plugins]` に足す

**完了条件**

- 実在するプラグインを 1 つ以上ロードして `describe` が成功する
  （**どのプラグインで確認したかを記録する**）
- 壊れたバンドル・版違い・ロード失敗が、落ちずにエラーとして表に出る
- **プラグインが無い状態で開いたプロジェクトが、ノードとパラメータを
  保ったまま残る**（`GraphDoc` の往復テスト。エラーは評価時のみ）
- **エラーが「プラグイン X が見つからない」と具体的に言う**
  （`EvalError::NoProcessorRegistered` のままにしない）
- 版差のパラメータ突き合わせが上表のとおりに振る舞う。とくに
  **インストール版に無いパラメータを捨てない**ことをテストで固定する
- 走査設定の 3 択が効き、既定が `background` である

### OFX-4 Image Effect Suite（CPU レンダー）

- `FrameBuffer` ⇄ OFX の画像（矩形 RGBA + RoD / ROI）の変換
- CPU レンダー経路で 1 プラグインが実際に絵を出す

**完了条件**

- OFX ノードを 1 つ挟んだ評価が CPU 経路で完走する
- **アルファ規約とチャンネル順が Ravel の既存ノードと一致する**
  （CPU 参照との比較テスト。`comp/*` の既存テストと同じ形）
- RoD / ROI が Ravel の bbox と矛盾しない

### OFX-5 Parameter Suite と Ravel UI への表示

- OFX のパラメータ型を Ravel のパラメータへ写像する（上の対応表）
- Properties パネルに出す。**写せない型は黙って落とさず明示する**
- **`PushButton` の機構**を決める（値ではなくアクション）
- **`Bytes` の置き場**を決める。`ParameterValue` に variant を足すなら
  永続化フォーマットの変更手順（`docs/dev/persistence.md`）に従う
- **`Group` / `Page`** のレイアウトを Properties パネルでどう出すか決める
- **プラグイン起点のパラメータ書き込み**を受け取り、undo に乗せる
- Dialog Suite（`RequestDialog` / `kOfxActionDialog`）を実装する

**完了条件**

- REQ-PLUGIN-001 の受入条件「プラグインパラメータが Ravel UI に表示される」
- 写像できない型の一覧が記録され、UI 上でもそうと分かる。
  **保存と往復だけは必ず成立する**（プラグインの UI で設定した内容が
  プロジェクトから消えない）
- パラメータ変更が再評価を起こし、undo に乗る
- **`PushButton` を押すとプラグインへアクションが届き、それが書き換えた
  パラメータが Ravel 側に反映され、undo 1 手で戻る**
- **プラグインが自前のウィンドウを開ける**（Dialog Suite 経由と、
  suite を使わず直接開く場合の両方。どのプラグインで確認したかを記録する）

### OFX-6a `interop` のインポート方向（Metal / D3D12 共通）

**この計画の土台。** `GPUBK-8` が置かなかったインポート方向をここで足す。
**Metal のためだけの作業ではない** — 共有可能なテクスチャを外部で確保して
Ravel 側で包む、という形が macOS（`IOSurface`）と Windows（`D3D12_HEAP_FLAG_SHARED`）
の**両方に要る**ので、`OFX-6b` と `OFX-7a` の共通の前提として分けてある。

- `ravel_gpu::interop` に「外部のバックエンド固有テクスチャを
  `GpuFrameBuffer` として取り込む」口を足す。`GPUBK-8` の
  エクスポート方向と対になる形にする
- Metal（`IOSurface` 由来の `MTLTexture`）と D3D12（共有ヒープの
  `ID3D12Resource`）の 2 実装。`GPUBK-8` と同じく**プラットフォームで
  API の形を変えない**
- `TexturePool` との関係を決める — 外部由来のテクスチャはプールが
  管理しないので、寿命の所有者を明示する

**完了条件**

- 外部で確保したテクスチャを包んだ `GpuFrameBuffer` が、通常のものと
  同じように評価経路を流れる
- safety 契約が `GPUBK-8` と同じ粒度で doc に書かれている
  （寿命の所有者、プールが関与しないこと、解放の責任）
- macOS 実機で、包んだテクスチャへの書き込みが元の `IOSurface` に見える
- Windows はコンパイル確認（CI）+ 実機確認（手順と結果を PR に記録）
- `gpu-interop-escape` lint の許可クレートに `ravel-ofx` が入る

### OFX-6b Metal GPU レンダー（macOS）

- `IOSurface` 経由でプロセス間の画像共有を成立させる
- `kOfxImageEffectPropMetalEnabled` / `…MetalCommandQueue` を立て、
  **Ravel と同じキューに積ませる**（`MED-GPU-07` でキューが取れるようになった）

**完了条件**

- macOS で GPU レンダー対応プラグインが**リードバック 0 回**で完走する
  （`TransferCounters::delta()` で機械的に確認。`GPUCOMP-*` と同じ手法）
- CPU 経路と GPU 経路の出力が一致する
- **どのプラグインで確認したかを記録する**

### OFX-7a CUDA GPU レンダー（Windows）

**Windows は準 1 級なので後回しにしない。** 共通の前提は `OFX-6a` だけで、
`OFX-6b` の完了は待たない。実際の着手順は Metal → CUDA
（開発機が macOS で反復が速く、`OFX-6a` の設計欠陥がそこで先に出るため。
ユーザー判断、2026-08-05）。

- `OFX-0` が選んだ橋（CUDA 外部メモリ、駄目なら OpenCL）を実装する
- `kOfxImageEffectPropCudaEnabled` / `…CudaStream` を立てる
- Ravel の D3D12 リソースを共有可能として確保し、CUDA 側へインポートさせる

**完了条件**

- Windows + NVIDIA の実機で GPU レンダー対応プラグインが
  **リードバック 0 回**で完走する
- CPU 経路と GPU 経路の出力が一致する
- **CI でコンパイル確認が走る**（GPU が無いので実行はされない。
  型とリンクの破壊は CI で落ちること）
- **実機確認の手順・機材・結果が PR に書かれている**
- CUDA が使えない環境（AMD / Intel GPU）で、落ちずに CPU 往復へ縮退し、
  そのことが `OFX-8` の表示に出る

### OFX-7b OpenCL GPU レンダー（Experimental）

AMD / Intel を GPU 経路に乗せるための追加経路。**CUDA の代替ではなく、
CUDA が成立した上での拡張**。手元に Radeon が無いので**開発機でも CI でも
検証されない** — 外部テスターの確認だけが根拠になる。

- `kOfxImageEffectPropOpenCLEnabled` / `…OpenCLCommandQueue` を立てる
- D3D12 → OpenCL の外部メモリ（OpenCL 3.0 拡張）で共有する
- **既定で無効。** 設定で明示的に有効化する

**完了条件**

- 「Experimental」の 4 条件（既定無効・外部検証・UI 表示・リリースを止めない）
  をすべて満たす
- **テスターによる実機確認の結果が PR に記録される**（機材と GPU を明記）
- 拡張が使えない環境で、有効化していても落ちずに CPU 往復へ縮退する
- CI でコンパイル確認が走る

### OFX-8 未対応 Suite と縮退の可視化

- 未実装 Suite の要求に `kOfxStatErrUnsupported` を返す
- **どのプラグインが GPU 経路に乗り、どれが CPU 往復に落ちるか**を
  ユーザーに見せる

**完了条件**

- REQ-PLUGIN-001 の受入条件「未対応 Suite に対して
  `kOfxStatErrUnsupported` が返される」
- プラグイン一覧に経路（GPU / CPU）と、CPU の場合はその理由が出る
- **黙って遅くなる経路が無い**

### OFX-9 文書更新

- **`REQ-PLUGIN.md` の 3 箇所を訂正する**（本計画「問題」節の 1〜3）。
  OpenGL を非対応として明記することを含む
- REQ-PLUGIN-001 / REQ-PROJ-002 の受入条件を実測に合わせる
- `docs/specifications/architecture.md` にホストプロセスを載せる
- `docs/dev/` にプラグインを追加・デバッグする手順を足す

## 検証

- `mise run check` と `mise run docs:check`
- **C++ 側は CI で macOS / Windows の両方をビルドする**。Rust 側と独立に
  失敗させる
- OFX ノードの出力は **CPU 参照との一致比較**で固定する（既存の GPU ノードと
  同じ形。保存画像のゴールデンは使わない）
- **リードバック回数を数える**（`TransferCounters`）。OFX ノードを挟んで
  回数が増えないことが `OFX-6b` / `OFX-7a` の本体
- サードパーティのプラグインを使うテストは **CI に置けない**（配布物を
  同梱できない）。手元確認の手順と対象プラグイン名を PR に書く
- 単位ごとに `ravel-review` を通してから PR を出す

## 落とし穴

- **プロセス境界の画像コピーがこの計画の失敗モード。** 素直に実装すると
  `HIGH-05` / `HIGH-04` で 0 にしたリードバックが OFX ノードごとに戻る。
  `OFX-0` で先に測るのはこのため
- **サードパーティのプラグインは行儀が悪い前提で書く。** 落ちる・固まる・
  仕様外の呼び出しをする。プロセス分離はそのためにある
- OFX の座標系と Y 軸の向き、premultiplied / unpremultiplied の規約は
  Ravel と一致するとは限らない。`OFX-4` の一致比較で早期に炙り出す
- **C++ 側にライセンス表記の義務が乗る**（vendoring した OFX ヘッダ）。
  `assets/fonts/` と同じく、パッケージング手順を作る人が引き取る

## 非対象

- **Multi-clip / Temporal Access / Interact Suite**（REQ-PLUGIN-001 の
  Phase A）。本計画は Phase B のコア Subset に閉じる。
  **Interact Suite（ビューア上にプラグインがオーバーレイを描き、マウスを
  受け取る）は Dialog Suite とは別物**で、こちらは非対象のまま — Ravel の
  Viewer に外部プロセスが描く経路が要り、`OVL-*`（オーバーレイ機構）と
  設計を合わせる必要があるので、単独で計画を立てる
- **CUDA / OpenCL / OpenGL バックエンドの実装**。`gpu-backend-plan.md` の
  非対象と同じ。CUDA の扱いは独立した要件に切る
- **ジオメトリ・属性・フィールドの OFX 経由での取り扱い**。REQ-PLUGIN-001 が
  「範囲の限界」として明記済み。Ravel 独自のプロシージャル機能は
  REQ-PLUGIN-002 の経路が担う
- **プラグインの配布・購入・ライセンス管理**

## 関連

- [`gpu-backend-plan.md`](gpu-backend-plan.md) — `GPUBK-8`（interop 出口）が
  この計画の前提。実装メモに OFX との突き合わせ表がある
- [`plugin-system-plan.md`](plugin-system-plan.md) — `PLUG-1`
  （`ProcessorRegistry`）に OFX ノードを登録する。REQ-PLUGIN-002 の
  ネイティブ経路とは別物
- `issues/closed/HIGH-05`、`issues/high/HIGH-04` — 再導入してはいけない
  リードバック経路
