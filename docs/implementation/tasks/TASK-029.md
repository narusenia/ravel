# TASK-029: OpenFXホスト基盤 + ネイティブプラグインAPI定義
- **マイルストーン**: MS6 Pro Features
- **関連要件**: REQ-PLUGIN-001, REQ-PLUGIN-002
- **規模**: L
- **依存タスク**: TASK-002, TASK-005

## 概要
OFXホストC/C++シムレイヤー（Rust→C FFI）、Suiteレジストリ（Image Effect、Parameter、GPU Render初期対応）、未実装Suiteへの`kOfxStatErrUnsupported`返却、プラグインスキャン・ロード、パラメータマッピング（OFXパラメータ→Ravel UI）を実装。加えてRavelネイティブプラグインAPI（Rustトレイトベース）の定義、プラグインマニフェスト仕様（TOML形式）の策定、両APIのドキュメント作成を行う。

## 実装ステップ
1. OFXホストC/C++シムレイヤー実装（Rust→C FFI）
2. Suiteレジストリ実装（Image Effect、Parameter、GPU Render初期対応）
3. 未実装Suiteに対して`kOfxStatErrUnsupported`を返却
4. プラグインスキャン・ロード機能実装
5. パラメータマッピング実装（OFXパラメータ→Ravel UI）
6. Ravelネイティブプラグインapi定義（Rustトレイトベース）
7. プラグインマニフェスト仕様策定（TOML形式）
8. 両APIのドキュメント作成

## 対象コンポーネント
- `crates/ravel-plugin/` (プラグインシステム)
- `crates/ravel-plugin/src/ofx/` (OFXホスト実装)
- `crates/ravel-plugin/src/ofx/ffi/` (C FFIバインディング)
- `crates/ravel-plugin/src/native/` (ネイティブプラグインAPI)
- `crates/ravel-plugin/src/manifest/` (マニフェスト仕様)

## 完了条件
- [ ] OFXホストC/C++シムレイヤーがビルド・動作する
- [ ] Image Effect、Parameter、GPU Render Suiteが登録・動作する
- [ ] 未実装Suiteが`kOfxStatErrUnsupported`を正しく返す
- [ ] OFXプラグインのスキャン・ロードが動作する
- [ ] OFXパラメータがRavel UIにマッピングされる
- [ ] RavelネイティブプラグインAPIがRustトレイトとして定義済み
- [ ] プラグインマニフェスト仕様（TOML形式）が策定済み
- [ ] 両APIのドキュメントが作成済み
