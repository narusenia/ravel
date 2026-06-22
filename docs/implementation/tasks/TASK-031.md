# TASK-031: Luaスクリプティング環境
- **マイルストーン**: MS6 Pro Features
- **関連要件**: REQ-PLUGIN-003, REQ-INFRA-007
- **規模**: M
- **依存タスク**: TASK-007

## 概要
mlua統合（io/osライブラリ除去によるサンドボックス化）、コンテキスト変数公開（frame、time、fps、プロジェクト設定）、数学関数公開（sin、cos、tan、noise、random）、ノードパラメータアクセスAPI公開、Luaコンソールパネル（GPUI）、シンタックスハイライト付きスクリプトエディタ、アニメーションチャネルでの式評価、サンドボックス制限（ファイル/ネットワークアクセス不可）の検証を実装する。

## 実装ステップ
1. mlua統合（サンドボックス: io/osライブラリ除去）
2. コンテキスト変数公開（frame、time、fps、プロジェクト設定）
3. 数学関数公開（sin、cos、tan、noise、random）
4. ノードパラメータアクセスAPI公開
5. Luaコンソールパネル実装（GPUI）
6. シンタックスハイライト付きスクリプトエディタ実装
7. アニメーションチャネルでの式評価実装
8. セキュリティ: サンドボックス制限の検証（ファイル/ネットワークアクセス不可）

## 対象コンポーネント
- `crates/ravel-scripting/` (スクリプティングエンジン)
- `crates/ravel-scripting/src/lua/` (Lua統合)
- `crates/ravel-scripting/src/sandbox/` (サンドボックス)
- `crates/ravel-ui/src/panels/lua_console/` (Luaコンソールパネル)
- `crates/ravel-ui/src/panels/script_editor/` (スクリプトエディタ)

## 完了条件
- [ ] mluaがサンドボックス化された状態で統合されている
- [ ] frame、time、fps、プロジェクト設定がLuaから参照可能
- [ ] sin、cos、tan、noise、randomの数学関数がLuaから利用可能
- [ ] ノードパラメータにLuaからアクセス可能
- [ ] GPUIベースのLuaコンソールパネルが動作する
- [ ] シンタックスハイライト付きスクリプトエディタが動作する
- [ ] アニメーションチャネルでLua式が評価される
- [ ] サンドボックス制限（ファイル/ネットワークアクセス不可）が検証済み
