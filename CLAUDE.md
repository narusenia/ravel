# Ravel — Claude Code ガイド

## プロジェクト概要

Ravelはタイムライン編集 + プロシージャルノードグラフを統合した動画編集ソフト。Rust + GPUI。

## アーキテクチャ原則

- **ノードグラフファースト**: 全処理はDAG上のノード。タイムラインはシーケンスノードの糖衣。
- **UI層と処理層の分離**: GPUI (UI) と Core Engine (DAG評価) は明確に分離。crossbeam-channelで通信。
- **Hybrid Pull評価**: 出力からPull + dirty通知でキャッシュ無効化。
- **イミュータブルデータ**: グラフは`Arc` + `im`クレートで構造共有。アンドゥ = バージョン切り替え。
- **32bit float内部処理**: 解像度/FPSに人工的上限なし。

## Cargoワークスペース構成

```
crates/
├── ravel-core/    # DAG評価、型システム、アニメーション、キャッシュ
├── ravel-gpu/     # wgpu計算パイプライン、シェーダ管理
├── ravel-media/   # FFmpeg統合、HWデコード、CPAL、OCIO
├── ravel-ui/      # GPUIパネル群（ノードグラフ、タイムライン、ビューア等）
└── ravel-app/     # アプリケーションエントリポイント、設定、i18n
```

## 技術スタック

- **UI**: GPUI (gpui_componentのdock/sheet活用)
- **GPU**: wgpu + Metal(macOS) / D3D11(Windows) ネイティブフォールスルー
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

## コミット

### 粒度
- 論理単位でコミット — 1コミット1概念
- 無関係な変更をまとめない
- 最後に一括コミットしない — 各論理単位の完了時にコミット
- 論理単位の例: 型/トレイト定義の追加、単一機能の実装、特定モジュールのテスト追加、バグ修正、設定/CI変更

### メッセージ
- 一行のみ（複数行禁止）
- 英語
- プレフィクス必須: `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, `chore:`, `perf:`, `ci:`
- 具体的に何を変更したか書く（`feat: add NodeData trait hierarchy and concrete types`）
- レビュー起因やフィードバック起因を書かない（`fix: codex review` NG）
- タスクID（TASK-001等）やissue番号をメッセージに含めない
- プレフィクス後は小文字

## プロジェクトファイル (.ravprj)

zipコンテナ。内部はRON/JSON/TOML。`graph/`にノードグラフ定義、`assets/`にアセット参照、`settings.toml`にプロジェクト設定。`.cache/`はzip化時に除外可能。

## テスト

- `cargo test`: ユニットテスト + インテグレーション
- リファレンス画像回帰テスト: レンダリング結果のピクセル比較
- `criterion`: パフォーマンスベンチマーク
- `cargo-fuzz`: .ravprjパーサ、OFXホストのファズテスト

## ドキュメント整合性

- 作業完了時、docs/ 配下の要件定義書・仕様書・実装計画書に影響する変更がないか確認し、必要なら更新するかユーザーに確認すること
