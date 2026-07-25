# TASK-008: プロジェクトファイル (.ravprj)
- **マイルストーン**: MS1 Foundation
- **関連要件**: REQ-PROJ-001, REQ-PROJ-003, REQ-PROJ-004, REQ-PROJ-005
- **規模**: M
- **依存タスク**: TASK-001, TASK-004

## 概要

Ravelのプロジェクトファイル形式`.ravprj`を実装する。RON形式によるノードグラフのシリアライズ/デシリアライズ、zipコンテナによるパッケージング、アセット参照システム、設定のオーバーライド階層、フォーマットバージョン管理とマイグレーション基盤を構築する。

## 実装ステップ

1. **RONシリアライズ/デシリアライズの実装**
   - `ron`クレートを依存に追加
   - TASK-001の`Graph`, `Node`, `Edge`, `Parameter`等に`Serialize`/`Deserialize`を導出
   - カスタムシリアライズ（`Arc<Node>` → 内部値のシリアライズ）
   - RON prettifier設定（git diff可能な読みやすいフォーマット）
   - `graph/main.ron`の読み書き
   - `graph/subgraphs/*.ron`の読み書き

2. **manifest.jsonの読み書き**
   - `serde_json`による`Manifest`構造体のシリアライズ/デシリアライズ
   - フィールド: `format_version: u32`, `ravel_version: String`, `project_name: String`, `created_at: DateTime`, `modified_at: DateTime`, `frame_rate: FrameRate`, `resolution: Resolution`, `color_config: String`
   - バージョン番号: 初期値`1`

3. **zipコンテナの作成/展開**
   - `zip`クレートで`.ravprj`ファイルの圧縮・展開
   - ディレクトリ構造: `manifest.json`, `graph/`, `timeline/`, `assets/`, `settings.toml`
   - `.cache/`と`.journal/`のzip除外
   - 作業中はワーキングディレクトリに展開して操作
   - 保存時にzipパッケージに圧縮
   - 大ファイル対応（ストリーミング圧縮/展開）

4. **アセット参照システムの実装**
   - `AssetRef`構造体: id, path, hash, metadata
   - `AssetPath` enum: `Relative(String)`, `Variable(var_name, rel_path)`
   - 変数展開: `${PROJECT_ROOT}`, `${ASSET_DIR}`等の環境変数的パス変数
   - `assets/refs.json`の読み書き
   - パス解決: 変数展開 → 相対パス → 絶対パスへの変換
   - 欠落アセットの検出と警告

5. **プロジェクト設定（TOML、オーバーライド階層）**
   - `toml`クレートで設定ファイルの読み書き
   - グローバル設定: OS別設定ディレクトリ（`dirs`クレートで解決）
     - macOS: `~/Library/Application Support/Ravel/`
     - Windows: `%APPDATA%\Ravel\`
     - Linux: `~/.config/ravel/`
   - グローバル設定ファイル: `general.toml`, `appearance.toml`, `keybindings.toml`, `performance.toml`, `plugins.toml`
   - プロジェクト設定: `settings.toml`（プロジェクト内）
   - オーバーライド解決順: デフォルト → グローバル → プロジェクト
   - 設定値の型安全なアクセスAPI

6. **フォーマットバージョンマイグレーションチェーン**
   - `MigrationFn`トレイト: `fn migrate(project_data: &mut ProjectData) -> Result<()>`
   - マイグレーション登録: バージョン番号 → マイグレーション関数のチェーン（v1→v2→v3→...）
   - マイグレーション実行: manifest.jsonのバージョンから最新バージョンまで順次適用
   - 初期バージョン: v1（マイグレーション関数なし、将来v2追加時の基盤）

7. **.ravprj.bak自動バックアップ（マイグレーション時）**
   - マイグレーション実行前に元ファイルを`.ravprj.bak`としてコピー
   - マイグレーション内容のログ出力（変更点の要約）
   - 不可逆な変更がある場合のフラグ付け（将来UIで警告表示に使用）

8. **グローバル設定パスの実装（dirsクレート）**
   - `dirs`クレートでOS別設定ディレクトリを解決
   - 設定ディレクトリの自動作成（初回起動時）
   - キャッシュディレクトリ、ログディレクトリのパス管理

9. **プロジェクトファイルパーサーのファズテスト**
   - `cargo-fuzz`でRONパーサーのファジング
   - 破損/不正な入力に対するパニック防止確認
   - manifest.jsonパーサーのファジング
   - ファズテストをCI nightlyに組み込み準備

## 対象コンポーネント

- `crates/ravel-core/src/project/mod.rs` — プロジェクト管理メインモジュール
- `crates/ravel-core/src/project/serialization.rs` — RONシリアライズ/デシリアライズ
- `crates/ravel-core/src/project/manifest.rs` — manifest.json読み書き
- `crates/ravel-core/src/project/container.rs` — zipコンテナ操作
- `crates/ravel-core/src/project/assets.rs` — アセット参照システム
- `crates/ravel-core/src/project/settings.rs` — 設定管理（TOML、オーバーライド）
- `crates/ravel-core/src/project/migration.rs` — バージョンマイグレーション
- `crates/ravel-core/src/project/paths.rs` — OS別パス管理（dirs）
- `crates/ravel-core/fuzz/` — ファズテスト

## 完了条件

- [ ] ノードグラフがRON形式でシリアライズ/デシリアライズできる
- [ ] RON出力がgit diffで有意義な差分を示す（整形済み出力）
- [ ] manifest.jsonの読み書きが動作する
- [ ] `.ravprj` zipコンテナの作成/展開が動作する
- [ ] `.cache/`と`.journal/`がzip化時に除外される
- [ ] アセット参照の変数展開（`${PROJECT_ROOT}`等）が動作する
- [ ] 欠落アセットが検出・警告される
- [ ] グローバル設定がOS別の適切なディレクトリに保存される
- [ ] プロジェクト設定がグローバル設定をオーバーライドする
- [ ] フォーマットバージョンがmanifest.jsonに記録される
- [ ] マイグレーション実行前に`.ravprj.bak`が作成される
- [ ] ファズテストがパニックを検出しない（初期実行）
- [ ] `.ravprj`のラウンドトリップテスト（保存→読み込みで同一グラフが復元）が通る
