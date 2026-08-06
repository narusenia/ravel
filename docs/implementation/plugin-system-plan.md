# プラグインシステム 実装計画（REQ-PLUGIN-002 / REQ-PLUGIN-004）

> **Status**: Planned — 2026-08-03

対象要件: REQ-PLUGIN-002（ネイティブプラグイン API）、REQ-PLUGIN-004
（プラグインマネージャ UI → オンラインレジストリ）。
関連: REQ-GPU-002、REQ-GPU-003、REQ-PROJ-006、REQ-INFRA-007、REQ-CORE-003。
前提計画: `done/exposed-parameters-plan.md`。

## 問題

**プロセッサの解決経路がハードコードされた `match` で、プラグイン空間が
宣言されているだけで存在しない。**

`processor_for_node`（`ravel-nodes/src/lib.rs:114`）の doc コメントは
「`type_key` が組み込みでないときは `None`（**plugin space**）」と自ら書いており、
呼び出し箇所は `ravel-app/src/eval_hooks.rs:105` の 1 つだけ。だが `None` の先に
何も無い。

一方 `NodeRegistry`（`registry/mod.rs:173`）は `templates: HashMap<String,
NodeTemplate>` しか持たず、プロセッサの解決を担わない。**ノードの「形」は
レジストリ、「中身」はハードコードされた `match` に分裂している。**

## 決定事項（2026-08-03 設計セッション）

### 第一形態は WGSL シェーダ + manifest

Rust dylib と WASM を先に置かない。理由は要件（REQ-PLUGIN-002 v2）に書いたが、
実装計画として効くのは 3 点。

1. **ABI の問題がゼロ。** `Arc<dyn NodeProcessor>` を dylib 境界で渡す設計を
   しなくてよい。配布物はテキスト
2. **ホスト側のサンドボックス機構が要らない。** WGSL はホストのファイル・
   ネットワーク・メモリに到達しない（REQ-INFRA-007 の段 1）。
   **ただし WGSL は `for` / `while` / `loop` を持つ**ので、GPU ハング / TDR は
   起こせる（`blur.wgsl:47` と `rasterize.wgsl:73` が実際にループを使い、
   `MED-GPU-03` が「per-pixel ループの無限膨張で GPU ハング」の実例）。
   **ループ境界の扱いは `PLUG-3` で決める。**
3. **宣言機構を新しく発明しない。** ノードの形（入出力ポート・パラメータ）は
   `done/exposed-parameters-plan.md` の `EXPO-1` が導入する宣言型を使う
   （本計画が独自の宣言形式を作らない、という制約）。REQ-GPU-003 の
   WGSL カスタムシェーダノードと同じ実行機構で、違いは「manifest で
   配布可能か」だけ

### 組み込みノードも同じ経路に寄せる

**組み込みだけを特別扱いすると、プラグイン経路が恒久的に二級市民になる。**
組み込みが `match` で解決され続ける限り、プラグイン経路の不具合は
組み込みノードのテストで検出されない。

`ProcessorRegistry` を作り、組み込みも起動時にそこへ登録する。
`processor_for_node` はレジストリの一実装（組み込みファクトリ群）になる。

### REQ-INFRA-009 と衝突させない

シェーダプラグインは WGSL を受け取る。REQ-INFRA-009 で wgpu を捨てても
**naga が WGSL → MSL / HLSL / SPIR-V を担うので、プラグインの契約は変わらない**。

ただし本計画がバインディングの記述に触るので、`gpu-backend-plan.md` の
`GPUBK-1`（バインディング記述をバックエンド非依存に）と**同じ型を使う**。
先に着手する側が型を定義し、後から来る側が乗る。順序はどちらでもよいが、
**2 つの記述型を作ってはいけない。**

### Rust ネイティブは配布対象にしない

Rust に安定 ABI が無く、同一コンパイラ前提でしか成立しない。
「自分用・社内用の拡張」として置き、`REQ-PLUGIN-004` のレジストリ配布からは
外す。信頼モデル（隔離なし）をユーザーに明示することが受入条件。

## 目標構成

```text
NodeRegistry          ノードの「形」（NodeTemplate）
ProcessorRegistry     ノードの「中身」（ファクトリ）  ← 新設
   ├── 組み込み          （現 processor_for_node を移設）
   ├── WGSL シェーダ      （manifest + .wgsl）        ← v1
   ├── WASM              （ジオメトリ処理）           ← v2
   └── Rust ネイティブ    （信頼モデル・配布対象外）    ← 補助

manifest（TOML、REQ-PLUGIN-004）
   └── ノードの形 = EXPO-1 の宣言型（done/exposed-parameters-plan.md）
```

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| PLUG-1 | `ProcessorRegistry` と組み込みの移設 | — |
| PLUG-2 | manifest 形式とスキャン・ロード | PLUG-1, `EXPO-1` |
| PLUG-3 | WGSL シェーダノード | PLUG-2, `GPUBK-1` |
| PLUG-4 | プラグインマネージャ UI | PLUG-3 |
| PLUG-5 | WASM ジオメトリノード | PLUG-2 |
| PLUG-6 | 文書更新 | PLUG-4 |

### PLUG-1 `ProcessorRegistry` と組み込みの移設

- ノードの `type_key` からプロセッサを作るファクトリのレジストリ
- 現 `processor_for_node` の `match` を「組み込みファクトリ群」として登録する
  形に移す。**振る舞いは変えない**
- 解決できない `type_key` のエラーが、現在の「黙って `None`」から
  「未知のノード種別」として報告される形になる

**完了条件**

- 全組み込みノードがレジストリ経由で解決され、既存テストが通る
- 未知の `type_key` が明示的なエラーになるテスト
- `eval_hooks.rs` の呼び出し側がレジストリを引くだけになる
- **組み込みとプラグインが同一の登録 API を使う**ことをテストで示す
  （テスト用の偽プラグインを登録して評価する）

### PLUG-2 manifest 形式とスキャン・ロード

- `REQ-PLUGIN-004` の TOML manifest を実装する。`type` は
  `node-pack` / `ofx-bundle` / `template` / `shader` / `lua-script` / `theme`
- **ノードの形の宣言部分は `EXPO-1` が導入する宣言型を使う**
  （本計画では新規に定義しない）
- プラグインディレクトリのスキャン、`ravel_compat` のバージョン照合
- 読み込み失敗（manifest 不正・互換性なし・ファイル欠落）が
  理由付きで報告され、アプリが落ちない

**完了条件**

- manifest から `NodeTemplate` が生成されるテスト
- 不正な manifest が理由付きで拒否されるテスト
- `ravel_compat` の不一致が拒否されるテスト
- 手動配置（ディレクトリに置く）で認識されるテスト
- 宣言が `EXPO-1` と同じ型・同じ検証を通ることのテスト

### PLUG-3 WGSL シェーダノード

- manifest で宣言された入出力テクスチャとユニフォームから、
  バインディング記述（`GPUBK-1` の型）を組む
- WGSL を naga で検証し、失敗を位置付きで報告する
- 宣言されたパラメータが Properties に出て、統一チャネル（REQ-CORE-007）に
  接続できる
- REQ-GPU-003（プロジェクト内のユーザーシェーダ）と実行機構を共有する

**完了条件**

- WGSL 1 本 + manifest 1 つでノードが増えるテスト
- 宣言したパラメータが Properties に出て、キーフレーム・式が付くテスト
- コンパイルエラーが位置付きで表示され、アプリが落ちないテスト
- シェーダノードを含むグラフの評価がゴールデンテストで安定すること
- **ループ境界の扱いが決まっていること。** 選択肢は (a) 静的解析で非有界
  ループを拒否、(b) ループ回数を決めるパラメータをクランプ（`MED-GPU-03` が
  blur で採った方法）、(c) TDR を受け入れる。**どれを採ったかを本計画に記録し、
  REQ-INFRA-007 の受入条件を満たす**
- プラグインシェーダがホストのファイル・ネットワーク・メモリに到達しないこと
- 実機でサンプルのシェーダプラグインを配置して動作確認

### PLUG-4 プラグインマネージャ UI

- インストール済みプラグインの一覧、有効 / 無効切り替え、バージョン表示
- **信頼モデルを種別ごとに表示する**（REQ-INFRA-007 の段）
- 読み込み失敗の理由表示
- コマンド・キーバインド・ロケール

**完了条件**

- 一覧・有効 / 無効・バージョンが表示される
- 信頼モデルが種別ごとに表示される
- 無効化したプラグインのノードが解決されず、既存プロジェクトが
  「未知のノード種別」として壊れずに開ける
- `assets/locales/{en,ja}.toml` にキーが揃っている

### PLUG-5 WASM ジオメトリノード

- Component Model（WIT）でジオメトリ処理のインターフェースを定義する
- 属性配列を線形メモリで渡す。ドメイン・型の対応を決める
- capability ベースの権限（REQ-INFRA-007 の段 3）

**完了条件**

- WASM モジュールがジオメトリ属性を変形するテスト
- ファイルシステム・ネットワークに到達できないテスト
- 実行時間とメモリの上限が効くテスト
- 属性の型・ドメインの往復が保たれるテスト

### PLUG-6 文書更新

- `REQ-PLUGIN-002` / `-004` の受入条件
- `docs/dev/` のノード追加手順に「プラグインとして追加する」経路を足す
- `docs/agent-api-reference.md` の `ProcessorRegistry`
- `docs/specifications/architecture.md` の拡張点

## 検証

- PLUG-1〜3、5 はヘッドレス（PLUG-3 は GPU アダプタを要する）
- PLUG-4 は実機
- **PLUG-1 の「組み込みとプラグインが同一 API」テストが本計画の背骨。**
  ここが分岐したままだとプラグイン経路が検証されない

## 非対象

- **OFX**（REQ-PLUGIN-001）。`gpu-backend-plan.md` の `GPUBK-8` の後
- **オンラインレジストリ**（`ravel install <package>`）。手動配置のみ
- **Rust ネイティブ dylib のロード**。信頼モデルの補助形態として要件には
  あるが、ABI 不安定性の回避策を決めていないので本計画では扱わない
- **サブグラフテンプレートの配布**（REQ-PLUGIN-005）。
  `done/exposed-parameters-plan.md` の `EXPO-6`
- **プラグインの署名・検証**
