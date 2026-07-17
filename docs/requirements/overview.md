# Ravel — 要件定義 概要

## プロジェクト概要

Ravelは、タイムラインベース編集とプロシージャルノードグラフを統合した次世代動画編集ソフトウェア。Houdini/Cavalryのようなプロシージャル生成能力、OpenFX互換のプラグインエコシステム、そしてリリックモーション/モーショングラフィックスに特化した強力なタイポグラフィエンジンを持つ。

## ターゲットユーザー

- モーショングラフィックスデザイナー（リリックビデオ/MV制作）
- 映像クリエイター（カラーグレーディング、VFX合成）
- プロシージャルアーティスト（ジェネラティブアート、データビジュアライゼーション）

## 技術スタック

- **言語**: Rust
- **UIフレームワーク**: GPUI (Zed由来)
- **GPU**: wgpu基盤 + プラットフォームネイティブフォールスルー
- **メディアI/O**: FFmpeg (LGPL, ダイナミックリンク) + ネイティブHWデコーダ
- **カラー**: OpenColorIO (OCIO)
- **スクリプティング**: Lua (mlua)
- **オーディオ**: CPAL + dasp/rubato

## ライセンスモデル

オープンコア — コアエンジン+基本ノード+プラグインAPIをOSS、プレミアム機能/テンプレート/サポートを商用レイヤーで提供。GPL依存は回避またはダイナミックリンク分離を徹底。

## スコープ一覧

| スコープ | 説明 | 要件数 |
|----------|------|--------|
| [CORE](REQ-CORE.md) | コアエンジン（DAG評価、型システム、スレッディング、キャッシュ、属性/フィールド） | 13 |
| [LAYER](REQ-LAYER.md) | レイヤーネットワークモデル（1レイヤー=1ノードネットワーク、殻、Layer Ref、サブネットワーク） | 11 |
| [GPU](REQ-GPU.md) | GPU計算パイプライン、シェーダ管理 | 3 |
| [UI](REQ-UI.md) | ユーザーインターフェース全般 | 10 |
| [MEDIA](REQ-MEDIA.md) | メディアI/O、オーディオエンジン | 3 |
| [MOGRAPH](REQ-MOGRAPH.md) | モーショングラフィックス、ジェネラティブ機能 | 5 |
| [DATA](REQ-DATA.md) | 外部データ駆動（テーブル入力、属性バインディング、ライブ入力） | 3 |
| [CODE](REQ-CODE.md) | コードベースジェネレーター（コードLayer、シーケンスAPI、ホットリロード） | 4 |
| [RENDER](REQ-RENDER.md) | レンダリング、エクスポート、カラーマネジメント | 3 |
| [PLUGIN](REQ-PLUGIN.md) | プラグインシステム、スクリプティング | 5 |
| [PROJ](REQ-PROJ.md) | プロジェクト管理、設定、リカバリ | 5 |
| [INFRA](REQ-INFRA.md) | インフラ（プラットフォーム、i18n、テスト、配布） | 8 |

## 全要件一覧

### CORE — コアエンジン

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-CORE-001 | ノードグラフトップレベルアーキテクチャ | Must | Revised (v3) |
| REQ-CORE-002 | Hybrid Pull評価エンジン | Must | Draft |
| REQ-CORE-003 | 階層型トレイトベース型システム | Must | Draft |
| REQ-CORE-004 | イミュータブルデータ構造によるアンドゥ | Must | Draft |
| REQ-CORE-005 | 専用スレッドプール + Tokio I/O | Must | Draft |
| REQ-CORE-006 | 三層キャッシュ (VRAM/RAM/Disk) | Must | Draft |
| REQ-CORE-007 | 統一アニメーションチャネル | Must | Draft |
| REQ-CORE-008 | マルチシーケンス + ネスト + ノード共有 | Should | Draft |
| REQ-CORE-009 | 制限なし解像度/FPS/32bit float内部処理 | Must | Draft |
| REQ-CORE-010 | ジオメトリ属性システム | Must | Draft |
| REQ-CORE-011 | ステートフル評価とシミュレーションキャッシュ | Must | Draft |
| REQ-CORE-012 | 汎用フィールド評価 | Must | Draft |
| REQ-CORE-013 | グラフ内反復 | Could | Draft |

### LAYER — レイヤーネットワークモデル

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-LAYER-001 | レイヤー = 殻 + ノードネットワーク | Must | Draft |
| REQ-LAYER-002 | ネットワークインターフェース（In / Out ノード） | Must | Draft |
| REQ-LAYER-003 | サブネットワーク | Must | Draft |
| REQ-LAYER-004 | ネットワーク内パラメータのアニメーション | Must | Draft |
| REQ-LAYER-005 | Layer Ref ノード（レイヤー間参照）と Null レイヤー | Must | Draft |
| REQ-LAYER-006 | レイヤーローカル時間評価 | Must | Draft |
| REQ-LAYER-007 | 評価モデル（殻コンパイル + ネットワーク再帰評価） | Must | Draft |
| REQ-LAYER-008 | レイヤーテンプレートと作成時ネットワーク生成 | Must | Draft |
| REQ-LAYER-009 | ネットワーク所有権と ID 体系 | Must | Draft |
| REQ-LAYER-010 | 調整レイヤー | Should | Draft |
| REQ-LAYER-011 | ノードエディタのネットワークコンテキスト UI | Must | Draft |

### GPU — GPU計算パイプライン

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-GPU-001 | wgpu基盤 + ネイティブフォールスルー | Must | Draft |
| REQ-GPU-002 | Hybridシェーダ管理 | Should | Draft |
| REQ-GPU-003 | WGSLカスタムシェーダノード | Should | Draft |

### UI — ユーザーインターフェース

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-UI-001 | GPUIフレームワーク採用 | Must | Draft |
| REQ-UI-002 | ノードグラフエディタ (自由配置+ガイド+階層化) | Must | Draft |
| REQ-UI-003 | リッチタイムライン + ノードグラフ展開 | Must | Revised (v3) |
| REQ-UI-004 | スコープ付きビューア + パネルトグル | Must | Draft |
| REQ-UI-005 | ワークスペースプリセット + カスタマイズ → フリードッキング | Must | Draft |
| REQ-UI-006 | テーマシステム + アクセシビリティ | Should | Draft |
| REQ-UI-007 | フルカスタマイズキーバインド + NLEプリセット | Should | Draft |
| REQ-UI-008 | メディアビン + メタデータ → スマートコレクション | Should | Draft |
| REQ-UI-009 | マルチモニタ + デタッチ + 専用ビューアウィンドウ | Should | Draft |
| REQ-UI-010 | ノード/クリップコピペ + ファイルD&D | Must | Draft |

### MEDIA — メディアI/O・オーディオ

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-MEDIA-001 | FFmpeg + ネイティブHWデコーダ | Must | Draft |
| REQ-MEDIA-002 | CPAL + DSPクレート オーディオエンジン | Must | Draft |
| REQ-MEDIA-003 | オーディオリアクティブ (FFT+ビート検出+BPM同期) | Should | Draft |

### MOGRAPH — モーショングラフィックス

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-MOGRAPH-001 | 基本シェイプ + インスタンス複製 + per-instance 変調 | Must | Revised (v2) |
| REQ-MOGRAPH-002 | パーティクル（ポイントジオメトリシミュレーション） | Should | Revised (v2) |
| REQ-MOGRAPH-003 | 3D基本機能 (テキスト押し出し、プリミティブ、カメラ) | Should | Draft |
| REQ-MOGRAPH-004 | プロシージャルタイポグラフィ | Must | Draft |
| REQ-MOGRAPH-005 | ビルトインエフェクトライブラリ | Must | Draft |

### DATA — 外部データ駆動

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-DATA-001 | テーブルデータ入力ノード | Should | Draft |
| REQ-DATA-002 | データ→属性バインディング | Should | Draft |
| REQ-DATA-003 | リアルタイム外部入力 | Could | Draft |

### CODE — コードベースジェネレーター

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-CODE-001 | コード Layer / コードノード | Should | Draft |
| REQ-CODE-002 | シーケンス糖衣 API | Should | Draft |
| REQ-CODE-003 | ホットリロード | Should | Draft |
| REQ-CODE-004 | 多言語ランタイム（WASM） | Could | Draft |

### RENDER — レンダリング・エクスポート

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-RENDER-001 | キューベースバックグラウンドレンダリング | Must | Draft |
| REQ-RENDER-002 | Write Nodeノード単位中間出力 | Should | Draft |
| REQ-RENDER-003 | OCIO + GPU LUTカラーマネジメント | Must | Draft |

### PLUGIN — プラグインシステム

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-PLUGIN-001 | OpenFX統合 (C/C++ Shim、B→Aフル準拠) | Must | Draft |
| REQ-PLUGIN-002 | ネイティブプラグインAPI (Rust/WASM) | Should | Draft |
| REQ-PLUGIN-003 | Luaスクリプティング (mlua) | Must | Draft |
| REQ-PLUGIN-004 | プラグインマネージャUI → オンラインレジストリ | Should | Draft |
| REQ-PLUGIN-005 | プリセット/テンプレートシステム + コミュニティ配布 | Should | Draft |

### PROJ — プロジェクト管理

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-PROJ-001 | .ravprj zipコンテナ プロジェクトファイル | Must | Draft |
| REQ-PROJ-002 | ジャーナル + プロセス分離 + 自動保存設定 | Must | Draft |
| REQ-PROJ-003 | 自動マイグレーション + バックアップ + 確認 | Must | Draft |
| REQ-PROJ-004 | カテゴリ別設定 + プロジェクトオーバーライド | Should | Draft |
| REQ-PROJ-005 | シングルユーザー + チームワークフロー考慮 | Should | Draft |

### INFRA — インフラストラクチャ

| ID | タイトル | 優先度 | ステータス |
|----|----------|--------|------------|
| REQ-INFRA-001 | macOSリード + Windows設計考慮、Linux後追い | Must | Draft |
| REQ-INFRA-002 | 英語+日本語、i18n基盤初期導入 | Must | Draft |
| REQ-INFRA-003 | 自動アップデート + チャネル制 | Should | Draft |
| REQ-INFRA-004 | フルテスト戦略 (ユニット+統合+回帰+ベンチ+ファズ) | Must | Draft |
| REQ-INFRA-005 | ユーザーガイド + API仕様書 | Should | Draft |
| REQ-INFRA-006 | ローカルログ + オプトイン匿名テレメトリ | Should | Draft |
| REQ-INFRA-007 | Luaサンドボックス + OFXプロセス分離 | Must | Draft |
| REQ-INFRA-008 | オープンコアライセンス (GPL依存回避) | Must | Draft |

## 用語集

| 用語 | 定義 |
|------|------|
| DAG | Directed Acyclic Graph。ノード間の依存関係を表す有向非巡回グラフ |
| ノードグラフ | データフロー型のビジュアルプログラミング環境 |
| タイムライン | 時間軸に沿ったクリップ配置による線形編集インターフェース |
| サブグラフ | 複数ノードをカプセル化した複合ノード。パラメータを外部公開可能 |
| 殻（シェル） | レイヤーの汎用プロパティ（時間配置・Transform・Opacity・blend_mode・親子付け等）の入れ物。ネットワークの外側で時間変換・合成を担う |
| サブネットワーク | ネットワーク内に作る入れ子グラフ。独自の In/Out（カスタムポート可）を持つ |
| Layer Ref | 同一コンポジション内の他レイヤーの Out ポートを参照するノード（`layer.ref`） |
| 所有パス | 評価インスタンスの名前空間を表すパス（CompId / LayerId / [SubnetNodeId ...] / NodeId）。評価キャッシュ・dirty のキー。ノード ID 自体はドキュメント内でグローバル一意 |
| ネットワーク境界ノード | 殻の合成チェーンとレイヤーネットワークを繋ぐノード。EvalContext をローカル時間に書き換えて内部を再帰評価する |
| シーケンスノード | タイムラインをノードグラフ上で表現するノード |
| Write Node | 任意ノードの出力をファイルに書き出すノード |
| 統一チャネル | パラメータの値ソース（キーフレーム/式/ノード出力等）を共通インターフェースで扱う仕組み |
| 属性 | ジオメトリ要素（ポイント/インスタンス等）に付与される任意名の値。ドメインと型を持つ |
| フィールド | 位置（および入力属性）から値への関数。属性変調・フォース・per-instance 変調の共通機構 |
| インスタンス | 複製ノードが生成する参照ベースの複製要素。per-instance 属性で個別変調可能 |
| simキャッシュ | ステートフルノードのフレーム逐次評価結果を蓄積するキャッシュ |
| ドメイン | 属性が付く単位。point / primitive / instance / geometry 全体 |
| OCIO | OpenColorIO。業界標準のカラーマネジメントライブラリ |
| OFX / OpenFX | 映像エフェクトプラグインの業界標準API |
| WGSL | WebGPU Shading Language。wgpuのシェーダ言語 |
| .ravprj | Ravelプロジェクトファイル。zip圧縮ディレクトリ |
| HWデコーダ | ハードウェアデコーダ（VideoToolbox, NVDEC, VAAPI等） |
| LUT | Look-Up Table。カラー変換テーブル |
| プロキシ | 低解像度の代替メディアファイル。編集時の負荷軽減に使用 |
