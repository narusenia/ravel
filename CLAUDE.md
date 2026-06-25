# Ravel — Claude Code ガイド

## プロジェクト概要

Ravelはタイムライン編集 + プロシージャルノードグラフを統合した動画編集ソフト。Rust + GPUI-CE。

## アーキテクチャ原則

- **ノードグラフファースト**: 全処理はDAG上のノード。タイムラインはシーケンスノードの糖衣。
- **UI層と処理層の分離**: GPUI-CE (UI) と Core Engine (DAG評価) は明確に分離。crossbeam-channelで通信。
- **Hybrid Pull評価**: 出力からPull + dirty通知でキャッシュ無効化。
- **イミュータブルデータ**: グラフは`Arc` + `im`クレートで構造共有。アンドゥ = バージョン切り替え。
- **32bit float内部処理**: 解像度/FPSに人工的上限なし。

## Cargoワークスペース構成

```
crates/
├── ravel-core/    # DAG評価、型システム、アニメーション、キャッシュ
├── ravel-gpu/     # wgpu計算パイプライン、シェーダ管理（wgpuはgpui-ceのフォーク版と統一）
├── ravel-media/   # FFmpeg統合、HWデコード、CPAL、OCIO
├── ravel-ui/      # ヘッドレスUI層（パネル分類、プリセット、メニュー、キーバインド）
└── ravel-app/     # GPUI-CEレンダリングホスト、Root/DockArea、アクション登録
```

## 技術スタック

- **UI**: GPUI-CE (gpui-ceコミュニティ版, gpui_componentのdock/sheet活用, フォーク版 `narusenia/gpui-component` gpui-ce-compatブランチ)
- **GPU**: wgpu (zed-industries/wgpuフォーク, gpui-ceと同一rev) + Metal(macOS) / D3D11(Windows) / Vulkan(Linux)
- **メディア**: FFmpeg (LGPLダイナミックリンク), VideoToolbox/NVDEC
- **オーディオ**: CPAL + dasp + rubato
- **カラー**: OpenColorIO (C++ FFI)
- **スクリプト**: Lua (mlua, サンドボックス)
- **シリアライズ**: RON (グラフ), JSON (メタデータ), TOML (設定)
- **並行処理**: rayon (評価), crossbeam (通信), tokio (I/Oのみ)
- **アンドゥ**: im クレート (persistent data structures)

## コーディング規約

- ライセンスヘッダ: Apache 2.0 / MIT dual license
- GPL依存禁止（ダイナミックリンク分離が不可能な場合）
- FFmpegは必ずダイナミックリンク
- `unsafe`はプラットフォーム固有コード（HWデコード、OFX FFI）に限定
- i18n: UIテキストは全て`t!`マクロ経由。ハードコード文字列禁止
- エラーハンドリング: `thiserror` + `anyhow`

## Git 規約

### ブランチ命名
- Conventional プレフィクス必須: `feat/`, `fix/`, `refactor/`, `docs/`, `test/`, `chore/`, `perf/`, `ci/`
- プレフィクス後はケバブケース（`feat/node-graph-evaluator`, `fix/timeline-crash`）
- 具体的な機能/修正名をつける — 抽象名禁止（`feat/phase1`, `fix/review-feedback` NG）
- 例:
  - `feat/dag-topological-sort`
  - `fix/wgpu-shader-compilation`
  - `refactor/split-media-pipeline`
  - `docs/update-architecture-guide`

### コミット粒度
- 論理単位でコミット — 1コミット1概念
- 無関係な変更をまとめない
- 最後に一括コミットしない — 各論理単位の完了時にコミット
- 論理単位の例: 型/トレイト定義の追加、単一機能の実装、特定モジュールのテスト追加、バグ修正、設定/CI変更

### コミットメッセージ
- 一行のみ（複数行禁止）
- 英語
- プレフィクス必須: `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`, `perf:`, `ci:`
- 具体的に何を変更したか書く（`feat: add NodeData trait hierarchy and concrete types`）
- レビュー起因やフィードバック起因を書かない（`fix: codex review` NG）
- タスクID（TASK-001等）やissue番号をメッセージに含めない
- プレフィクス後は小文字

### PR タイトル
- コミットメッセージと同じフォーマット（Conventional プレフィクス + 具体名）

## プロジェクトファイル (.ravprj)

zipコンテナ。内部はRON/JSON/TOML。`graph/`にノードグラフ定義、`assets/`にアセット参照、`settings.toml`にプロジェクト設定。`.cache/`はzip化時に除外可能。

## テスト

- `cargo test`: ユニットテスト + インテグレーション
- リファレンス画像回帰テスト: レンダリング結果のピクセル比較
- `criterion`: パフォーマンスベンチマーク
- `cargo-fuzz`: .ravprjパーサ、OFXホストのファズテスト

## gpui-ce 移行メモ

- gpui (Zed) → gpui-ce (community edition) に移行済み。blade-graphics削除、wgpu統一バックエンド。
- `gpui_platform::application()` でブートストラップ。`Application::new()` は存在しない。
- ウィンドウのルートビューは `gpui_component::Root` で包むこと（テーマ色、フォント、rem_size、通知レイヤー等を管理）。
- `gpui_platform` の `font-kit` フィーチャーを有効にしないとテキストが描画されない（`NoopTextSystem` になる）。
- `QuitMode::Default` は macOS では `Explicit`（ウィンドウ閉じてもプロセス存続）。`QuitMode::LastWindowClosed` を明示設定。
- `gpui_component::init(cx)` 後に `Theme::sync_system_appearance(None, cx)` を呼ばないとライトテーマがデフォルトになる。
- gpui-component は `narusenia/gpui-component` フォークの `gpui-ce-compat` ブランチを使用。`[patch]` セクションで gpui を gpui-ce に差し替え。
- macOS の描画は Metal 直接（gpui_macos）。gpui_wgpu は Linux/Windows のみ。

## ドキュメント整合性

- 作業完了時、docs/ 配下の要件定義書・仕様書・実装計画書に影響する変更がないか確認し、必要なら更新するかユーザーに確認すること
