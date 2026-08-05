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

### 要件書の記述を 2 箇所訂正する

上の実測により `docs/requirements/REQ-PLUGIN.md` に誤りが見つかった。
`OFX-9` で直す（この計画で勝手に書き換えず、単位として持つ）。

1. 「初期サポート (Phase B) … **GPU Render Suite (OpenGL/Metal/CUDA)**」 —
   **OpenCL が抜けている**。ヘッダは 4 つ定義している
2. 「REQ-INFRA-009 は Metal / D3D12 を直接触る抽象を作るので、**その抽象が
   OFX が要求する texture / device pointer をそのまま露出できる**」 —
   **D3D12 について偽**。露出はできるが OFX に渡す先が無い

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

**その消費者がこの計画。** `OFX-6` でインポート方向を `interop` に足す。

> **未検証**: `MTLDevice.newTexture(descriptor:iosurface:plane:)` と
> wgpu の `create_texture_from_hal` を繋いで同一 `IOSurface` を両プロセスから
> 包めることは、`OFX-0` のプロトタイプで**実際に確かめる**。ここが崩れると
> macOS のゼロコピーも崩れるので、コードを書く前に確認する。

### Windows と Linux は「まず CPU 往復、判断は測ってから」

繋がらない以上、初期は CPU 往復に落とす。**そのコストを測ってから**次を決める。
候補は 2 つあり、どちらも本計画の初期スコープ外:

- **D3D12 → CUDA 外部メモリ**（`cudaExternalMemoryHandleTypeD3D12Resource`
  相当）。NVIDIA 限定になる
- **D3D12 → OpenCL 外部メモリ**（OpenCL 3.0 の external memory 拡張）。
  ベンダ中立だが対応状況がまちまち

> **未検証**: 上記 2 つの API がそれぞれ現実に使える形で存在するかは
> **`OFX-0` で確認する**。ここでは「候補」として挙げるにとどめる。

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
| **Metal** | 不要（macOS は Metal で描いている） | macOS の GPU 対応プラグインは Metal | **`OFX-6` で実装する** |

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
| OFX-6 | Metal GPU レンダー（macOS）と `interop` のインポート方向 | `ravel-gpu`, 両方 | OFX-4 |
| OFX-7 | `OFX-0` の判定を実装（Windows / Linux の GPU 経路） | 両方 | OFX-0 の判定, OFX-6 |
| OFX-8 | 未対応 Suite の `kOfxStatErrUnsupported` と縮退の可視化 | 両方 + `ravel-ui` | OFX-5 |
| OFX-9 | 文書更新（REQ-PLUGIN-001 の 2 箇所の訂正を含む） | 要件・仕様・`docs/dev/` | OFX-8 |

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
- Windows / Linux の GPU 経路を **実施 / 見送り** のいずれかに判定し、
  根拠を本節に追記する。見送るなら `OFX-7` を ❌ にする
- macOS のゼロコピーが成立しない場合、**`OFX-6` の設計をやり直す**。
  この計画の他の単位は影響を受けない
- CUDA の要件定義に渡す入力（往復コストと橋渡しの可否）が揃う

### OFX-1 `ofx-host` の骨格と CMake ビルド

- `ofx-host/` に CMake プロジェクトを作る。成果物は実行ファイル 1 つ
- OFX ヘッダを `ofx-host/vendor/openfx/` に取り込む。**取り込み元のコミット
  ハッシュを `vendor/openfx/README.md` に記録する**
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
- ホストが落ちたら該当ノードの評価を失敗として返し、再起動する

**完了条件**

- ホストプロセスを外から `kill` しても Ravel が落ちず、
  該当ノードだけがエラーになる統合テスト
- 再起動後に同じノードが再び評価できる
- **プロトコルのバージョン不一致を検出して拒否する**
  （ホストと Ravel の版ずれは配布事故として必ず起きる）

### OFX-3 バンドルの走査・ロードと Property / Memory Suite

- 標準パス（macOS / Windows）の `*.ofx` バンドルを走査する
- `OfxGetNumberOfPlugins` / `OfxGetPlugin` を呼び、`describe` まで通す
- ホストが提供する側の Property Suite と Memory Suite を実装する

**完了条件**

- 実在するプラグインを 1 つ以上ロードして `describe` が成功する
  （**どのプラグインで確認したかを記録する**）
- 壊れたバンドル・版違い・ロード失敗が、落ちずにエラーとして表に出る

### OFX-4 Image Effect Suite（CPU レンダー）

- `FrameBuffer` ⇄ OFX の画像（矩形 RGBA + RoD / ROI）の変換
- CPU レンダー経路で 1 プラグインが実際に絵を出す

**完了条件**

- OFX ノードを 1 つ挟んだ評価が CPU 経路で完走する
- **アルファ規約とチャンネル順が Ravel の既存ノードと一致する**
  （CPU 参照との比較テスト。`comp/*` の既存テストと同じ形）
- RoD / ROI が Ravel の bbox と矛盾しない

### OFX-5 Parameter Suite と Ravel UI への表示

- OFX のパラメータ型を Ravel のパラメータへ写像する
- Properties パネルに出す。**写せない型は黙って落とさず明示する**

**完了条件**

- REQ-PLUGIN-001 の受入条件「プラグインパラメータが Ravel UI に表示される」
- 写像できない型の一覧が記録され、UI 上でもそうと分かる
- パラメータ変更が再評価を起こし、undo に乗る

### OFX-6 Metal GPU レンダー（macOS）と `interop` のインポート方向

**この計画の山場。** `GPUBK-8` が置かなかったインポート方向をここで足す。

- `ravel_gpu::interop` に「外部のバックエンド固有テクスチャを
  `GpuFrameBuffer` として取り込む」口を足す。`GPUBK-8` の
  エクスポート方向と対になる形にする
- `IOSurface` 経由でプロセス間の画像共有を成立させる
- `kOfxImageEffectPropMetalEnabled` / `…MetalCommandQueue` を立て、
  **Ravel と同じキューに積ませる**（`MED-GPU-07` でキューが取れるようになった）

**完了条件**

- macOS で GPU レンダー対応プラグインが**リードバック 0 回**で完走する
  （`TransferCounters::delta()` で機械的に確認。`GPUCOMP-*` と同じ手法）
- CPU 経路と GPU 経路の出力が一致する
- インポート方向の safety 契約が `GPUBK-8` と同じ粒度で doc に書かれている
- `gpu-interop-escape` lint の許可クレートに `ravel-ofx` が入る

### OFX-7 Windows / Linux の GPU 経路

`OFX-0` の判定が「実施」の場合のみ着手する。見送りなら ❌ にして、
CPU 往復のままであることを `OFX-8` の縮退表示に載せる。

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

- **`REQ-PLUGIN.md` の 2 箇所を訂正する**（本計画「問題」節の 1 と 2）
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
  回数が増えないことが `OFX-6` の本体
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
  Phase A）。本計画は Phase B のコア Subset に閉じる
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
