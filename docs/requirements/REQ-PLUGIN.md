# REQ-PLUGIN — プラグインシステム

## REQ-PLUGIN-001: OpenFX統合 (C/C++ Shim、B→Aフル準拠)

- **優先度**: Must
- **ステータス**: Draft
- **説明**: OpenFXプラグインをC/C++ Shimレイヤー経由で統合。初期はコアSubset（Image Effect + Parameter + GPU Render Suite）をサポートし、主要フィルタ系プラグイン（Sapphire、BorisFX、Neat Video等）の基本機能が動作。段階的にMulti-clip、Temporal Access、Interact Suiteを追加しOFX 1.4フル準拠を目指す。未実装Suiteは`kOfxStatErrUnsupported`を返す。OFXプラグインはREQ-PROJ-002のプロセス分離で隔離実行。
- **初期サポート (Phase B)**:
  - Image Effect Suite
  - Parameter Suite
  - GPU Render Suite (OpenGL/Metal/CUDA)
- **将来追加 (Phase A)**:
  - Multi-clip Suite
  - Temporal Access Suite
  - Interact Suite (ビューポートオーバーレイUI)
- **受入条件**:
  - [ ] OFXプラグインをスキャン・ロードできる
  - [ ] Image Effectプラグインが動作する
  - [ ] プラグインパラメータがRavel UIに表示される
  - [ ] GPU Renderが動作する
  - [ ] プラグインが別プロセスで隔離実行される
  - [ ] 未対応Suiteに対して`kOfxStatErrUnsupported`が返される
- **依存**: REQ-GPU-001, REQ-PROJ-002, REQ-INFRA-007

## REQ-PLUGIN-002: ネイティブプラグインAPI (Rust/WASM)

- **優先度**: Should
- **ステータス**: Draft
- **説明**: Ravelネイティブのプラグインシステム。OFXのC API制約に縛られず、Ravelのトレイトベース型システム（REQ-CORE-003）と直接統合。Rustネイティブプラグイン（信頼モデル）およびWASMプラグイン（サンドボックス実行）をサポート。将来WASMプラグインのCapability-based権限制御に拡張可能。
- **受入条件**:
  - [ ] Rustネイティブプラグインを作成・ロードできる
  - [ ] プラグインAPIドキュメントが提供される
  - [ ] プラグインがRavelの型システムと統合される
  - [ ] （将来）WASMプラグインがサンドボックス内で実行される

## REQ-PLUGIN-003: Luaスクリプティング (mlua)

- **優先度**: Must
- **ステータス**: Draft
- **説明**: `mlua`クレートでLuaを組み込み。パラメータエクスプレッション（`amplitude * sin(frame * frequency + phase)`等）、バッチ処理スクリプト、操作自動化に使用。Luaスクリプトはサンドボックス内で実行（`io`/`os`ライブラリ除外）。将来必要に応じて軽量エクスプレッションDSLやWASM拡張を検討。
- **受入条件**:
  - [ ] パラメータにLuaエクスプレッションを記述できる
  - [ ] `frame`, `time`, `fps`等のコンテキスト変数にアクセスできる
  - [ ] 数学関数（sin/cos/noise等）が利用できる
  - [ ] 他ノードのパラメータ値を参照できる
  - [ ] Luaコンソール/スクリプトエディタが提供される
  - [ ] ファイルシステム/ネットワークアクセスが制限される
- **依存**: REQ-INFRA-007

## REQ-PLUGIN-004: プラグインマネージャUI → オンラインレジストリ

- **優先度**: Should
- **ステータス**: Draft
- **説明**: アプリ内プラグインマネージャUI。インストール済みプラグイン（OFX/ネイティブ/WGSL/Lua/テーマ/テンプレート）の一覧表示、有効/無効切り替え、バージョン管理。パッケージmanifest仕様を初期から定義。将来的にオンラインレジストリ（`ravel install <package>`）に拡張。
- **パッケージmanifest形式**:
  ```toml
  [package]
  name = "glitch-effects"
  version = "1.0.0"
  type = "node-pack"  # node-pack | ofx-bundle | template | shader | lua-script | theme
  ravel_compat = ">=0.1.0"
  ```
- **受入条件**:
  - [ ] インストール済みプラグインの一覧が表示される
  - [ ] プラグインの有効/無効を切り替えられる
  - [ ] パッケージmanifest仕様が定義されている
  - [ ] 手動インストール（ディレクトリ配置）でプラグインが認識される

## REQ-PLUGIN-005: プリセット/テンプレートシステム + コミュニティ配布

- **優先度**: Should
- **ステータス**: Draft
- **説明**: 3粒度のプリセット。(1)ノード単位プリセット（パラメータ保存/復元）、(2)サブグラフテンプレート（複数ノードの再利用、公開パラメータ定義）、(3)プロジェクトテンプレート（SNS向け縦動画、MV用等）。テンプレートパッケージはREQ-PLUGIN-004のmanifest形式に準拠。ローカルインポート/エクスポートで初期運用。
- **受入条件**:
  - [ ] ノード単位のプリセットを保存/適用できる
  - [ ] サブグラフをテンプレートとして保存/インポートできる
  - [ ] プロジェクトテンプレートから新規プロジェクトを作成できる
  - [ ] テンプレートのエクスポート/インポートが可能
- **依存**: REQ-PLUGIN-004, REQ-UI-002
