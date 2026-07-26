# メディアインポート + アセット管理実装計画（REQ-UI-008 / REQ-UI-010 / REQ-PROJ-001）

> **Status**: In progress — 2026-07-26 設計確定。単位 1・2 実装済み

## 問題

メディアは**モデルには入っているが UI から一切使えない**。

- `Document::media_assets`（`id → MediaAssetEntry { path }`）に書き込む経路が
  UI に存在しない。`Document::with_media_asset` の呼び出しはテストと
  `project/mod.rs` のテストフィクスチャだけ。
- `CommandId::LayerAddVideo` は `assets/layer-templates/video.ron` を
  インスタンス化するが、`video` ノードの `asset_id` は未設定のままなので
  評価は `video: asset_id is not set` で失敗する。**動画を 1 本も開けない。**
- `MediaBin` パネルは `PlaceholderPanel`（`docs/ui-impl-status.md` で 🔲）。
- パスは絶対パスのみ。`AssetPath`（`Relative` / `Absolute` / `Variable`）と
  `AssetMetadata` / `ProxyInfo` は `crates/ravel-app/src/project/asset.rs` に
  実装済みだが**誰も使っておらず**、`assets/refs.json` は空のコレクションが
  書かれるだけ（`project/mod.rs` の doc コメントに明記されている）。
  REQ-PROJ-001 の「アセット参照が相対パスで記録される」は未達。
- `video` ノードは FFmpeg の `MediaReader` 専用。`ravel-media` の
  `image_seq::{detect_sequence, read_image_frame}` はどのノードからも
  呼ばれておらず、静止画・画像シーケンスの経路が無い。

要件は REQ-UI-008（メディアビン + メタデータ、Should）、REQ-UI-010（OS からの
ファイル D&D でインポート、Must）、REQ-PROJ-001 / REQ-PROJ（相対・変数パス）。

## 決定事項（2026-07-26 設計セッション）

| # | 決定 | 根拠 |
|---|---|---|
| 1 | `MediaAssetEntry` を `{ path: AssetPath, metadata: AssetMetadata, #[serde(skip)] resolved: Option<PathBuf> }` に拡張。永続は相対/変数、実行時の絶対パスは app が注入 | `EvalContext` は `Copy` のフレーム軸専用構造体で `PathBuf`/`HashMap` を持てない。評価用と保存用に `Document` を分けると `Arc::ptr_eq` による observer/undo の同一性判定が壊れる |
| 2 | 動画・静止画・画像シーケンスを **1 ノードに統合**（`type_key` を `media` にし `video` を alias として永続互換） | `is_time_dependent(&self)` は引数を取らないので 1 processor 型で出し分けられない。time-dependent のまま静止画を Arc キャッシュすれば実コストはゼロ |
| 3 | サムネイル/波形キャッシュは**外部グローバルキャッシュ**（`global_config_dir()/cache/`、キー = 絶対パス + mtime + size） | 未保存プロジェクトでも効く。`.ravprj` は zip なので同封すると保存サイズと保存時間、保存のバイト決定性にサムネ生成が絡む |
| 4 | メディアレイヤーの時間配置は **素材長 + 再生ヘッド位置**（`out_frame = ceil(duration_secs × comp_fps)`、長さ不明はコンプ全長にフォールバック） | 連続投入でヘッドに積める。`video` ノードは `media_frame_for(ctx.time, stream)` で秒基準にマップするので素材 fps ≠ コンプ fps でもズレない（REQ-LAYER-006） |
| 5 | 空コンプにメディアを入れてもコンプ設定は**変えない**。代わりに MediaBin に「素材からコンポジションを作成」を置く | 解像度/fps の暗黙書き換えは意外な破壊的変更 |
| 6 | オフラインは**検出 + 手動再リンク**まで v1。一括再リンク・ディレクトリ探索は v2 | 再リンクは「asset の path を差し替える Document 編集 1 回」なので小さい |
| 7 | オフライン/デコード失敗のレイヤーは**透明フレームとして継続**。評価全体を失敗させない | 大きなコンポジションで 1 枚壊れると全部見えなくなるのを避ける |
| 8 | アセットのメタデータ表示とパス編集は `PropertiesTarget::MediaAsset { id }` を追加して Properties に出す。MediaBin は一覧列のみ | `Composition` ターゲットの前例と同型。「ターゲットは identify するだけ、値は毎回ドキュメントから解決」規約に乗る |

## 目標アーキテクチャ

### アセットモデル（ravel-core）

`AssetPath` / `AssetMetadata` / `AssetKind` を `ravel-app` から
`crates/ravel-core/src/composition/asset.rs` へ移し、`MediaAssetEntry` を拡張する。

`AssetPath` は**単一文字列**として永続化する（`${` を含めば `Variable`、
絶対なら `Absolute`、それ以外は `Relative`）。v3 の
`MediaAssetEntry { path: PathBuf }` がそのまま `Absolute` として読めるため、
文書側の v3 → v4 マイグレーションが不要になる。

```rust
pub enum AssetPath { Absolute(PathBuf), Relative(String), Variable(String) }

pub enum AssetKind {
    /// FFmpeg で開けるコンテナ（動画 + 任意の音声ストリーム）。
    Container,
    /// 単一画像。デコード結果はプロセッサ内でキャッシュする。
    Still,
    /// 連番画像。prefix/suffix/padding と範囲を保持する。
    Sequence { prefix: String, suffix: String, padding: usize, start: u64, end: u64 },
}

pub struct MediaAssetEntry {
    pub path: AssetPath,          // 永続（相対/変数/絶対）
    pub kind: AssetKind,          // 永続
    pub metadata: AssetMetadata,  // 永続（w/h/fps/codec/duration/audio 有無）
    #[serde(skip)]
    pub resolved: Option<PathBuf>, // 実行時のみ。app が注入。None = オフライン
}
```

- **解決の責務は `ProjectState`**。`load_project_from` / インポート / `Save As`
  （プロジェクトルートが変わる）の後に `resolved` を埋めた `Document` を作る。
  変数テーブルは `${PROJECT_ROOT}` を既定で持つ（`asset::expand_variables` を core へ移動）。
- `media` ノードは `resolved` だけを見る。`None` なら**透明フレーム**を返し、
  `tracing::warn!` で 1 回だけ記録する（毎フレーム吐かない）。
- 画像シーケンスは **1 アセット = 1 シーケンス**。`AssetPath` は代表フレーム
  （先頭フレーム）を指し、実フレームは `AssetKind::Sequence` から組み立てる
  （`ImageSequenceInfo::frame_path` 相当のロジックを core に持つ）。
  シーケンスの fps は素材から取れないので `metadata.frame_rate` を既定
  コンプ fps で埋め、Properties で変更可能にする。

### 永続化

`format_version` 3 → 4 のマイグレーションを 1 段追加する
（`project/migration.rs` の既存チェーンに追加）。

- v3 の `media_assets`（`id → { path: PathBuf }`、絶対パス）を読み、
  `AssetPath::Absolute` + `kind` 推定（拡張子）+ `metadata` 空で v4 に持ち上げる。
  probe はロード時に行わない（I/O をロードに載せない）。メタデータは
  MediaBin が遅延で埋める。
- 保存時、プロジェクトルート配下のパスは `AssetPath::Relative` に、
  それ以外は `Absolute` のまま書く（`Variable` はユーザーが Properties で
  明示的に設定したときだけ）。
- `assets/refs.json` は**書かなくなる**（v4）。読み側は v3 以前の互換のため
  残す（現状も空なので情報損失は無い）。`AssetCollection` / `AssetRef` /
  `ProxyInfo` は `project/asset.rs` から削除し、プロキシは将来
  `MediaAssetEntry` の予約フィールドとして再導入する。

### インポート経路

```text
File ▸ Import…（CommandId::FileImport）      OS からのファイル D&D
        │                                            │
        └──────────────┬─────────────────────────────┘
                       ▼
      ravel-app: import::import_paths(paths, cx)
                       │  ravel-media::probe / detect_sequence（background executor）
                       ▼
      Document 編集 1 回 = commit_document → 1 undo
                       │
                       ├─ media_assets に追加（相対化 + kind + metadata）
                       └─ MediaBin が observe して行を出す
```

- probe は**バックグラウンド**で行い、UI スレッドをブロックしない
  （`.agents/rules/rust.md`「ブロッキング I/O を UI スレッドに乗せない」）。
- 複数ファイルの D&D は**まとめて 1 つの `commit_document`** = 1 undo。
- 同じ絶対パスのアセットが既にあれば再利用し、id を重複させない。
- probe に失敗したファイルはインポートしない。理由をログとダイアログの
  サマリで返す（`n 件中 m 件をインポート`）。

### MediaBin

- ヘッドレス側 `crates/ravel-ui/src/panels/media_bin.rs`（行モデル、種別
  フィルタ、検索）+ GPUI 側 `crates/ravel-app/src/panels/media_bin.rs`
  （Outliner / Timeline と同じ分割）。
- 行は `MediaBinRow { asset_id, name, kind, duration, offline }` の平坦なリスト。
  `Render` 内で probe / デコード / グラフ走査をしない。
- **選択は durable Global** `MediaSelection { assets: Vec<AssetId> }`。
  パネル内部に選択を持たない（`CanvasSelection` / `LayerSelection` と同じ方式。
  パネル内部選択の二重管理を廃した #151 / REQ-UI-013 の判断を踏襲）。
  選択変更時に `SelectedPropertiesTarget` へ `MediaAsset { id }` を書く。
- 行の操作: ダブルクリック / 右クリック → 「レイヤーとして追加」
  「素材からコンポジションを作成」「Relink…」「プロジェクトから削除」。
  削除は**使用中なら確認**（どのコンプの何レイヤーが参照しているかを数えて出す）。
- 音声アセットも同じパネルに出す（種別フィルタ = 全て / 映像 / 静止画 / 音声）。
  音声バンクとしてのタグ・カテゴリは `docs/implementation/audio-plan.md` 側。

### サムネイル

- `ravel-app` の `media/thumbnail.rs`。キー = `sha256(絶対パス + mtime + size)`、
  保存先 `global_config_dir()/cache/thumbnails/<key>.png`、メモリ LRU を前段に置く。
- 生成は background executor。動画は `duration × 0.1` 位置の 1 フレーム、
  静止画/連番は先頭フレームを 256px 長辺に縮小。
- 生成失敗（コーデック非対応など）は種別アイコンにフォールバックし、
  失敗をキャッシュして再試行ループにしない。

## 実装単位

各単位が 1 PR。単位 1 は他の全単位の前提。

1. **アセットモデルの相対/変数化**（ravel-core / ravel-app）— ✅ 実装済み
   `AssetPath` / `AssetKind` / `AssetMetadata` を core へ移動、
   `MediaAssetEntry` 拡張、`resolved` 注入経路（`ProjectState`）、
   format v3 → v4 マイグレーション、`refs.json` 書き込み停止。
   `video` ノードは `resolved` を読むように変更（挙動は同じ）。
   REQ-PROJ-001 の受入条件「相対パスで記録される」を満たす。
2. **`media` ノード統合**（ravel-nodes / assets）— ✅ 実装済み
   `type_key` を `media` にし `video` を alias 登録、`AssetKind` 分岐、
   静止画のデコード結果 Arc キャッシュ、連番の `frame_path` 読み、
   `resolved == None` で透明フレーム。レイヤーテンプレートを
   `video.ron` → `media.ron`（`LayerAddVideo` のラベルは維持）。
3. **インポート経路**（ravel-app / ravel-ui / assets/locales）
   `CommandId::FileImport` + File メニュー + OS ファイル D&D、
   background probe、相対化して `media_assets` へ 1 undo で追加、
   「レイヤーとして追加」（素材長 + 再生ヘッド）。
4. **MediaBin パネル**（ravel-ui / ravel-app）
   行モデル + 種別フィルタ + 検索、`MediaSelection` Global、
   `SelectedPropertiesTarget` 連携、行コンテキストメニュー、
   参照数つき削除確認、`PlaceholderPanel` の置換。
5. ✅ **サムネイル生成とキャッシュ**（ravel-app）
   外部キャッシュ + メモリ LRU + background 生成 + 失敗フォールバック。
6. **Properties の MediaAsset ターゲットと再リンク**（ravel-ui / ravel-app）
   — **単位 1 からの持ち越し**: 変数パスを設定できるようにするなら、
   `Save As` 後に live document の `resolved` を再解決する経路も同時に入れる
   （undo ステップにも dirty 化にもしないこと）。
   `PropertiesTarget::MediaAsset { id }`、メタデータ表示、
   パス編集（Absolute / Relative / Variable の切替）、
   `Relink…`（ファイルダイアログ → パス差し替え → 1 undo）、
   オフライン表示。
7. **オフラインの見え方とドキュメント整合**
   Outliner / Timeline のレイヤー行にオフライン印、
   `docs/ui-impl-status.md` / `docs/agent-api-reference.md` /
   `docs/specifications/data-model.md` / REQ-UI-008・REQ-UI-010・REQ-PROJ の更新。

## 完了条件（単位別）

- **単位 1**: 相対パスで保存された v4 プロジェクトを別ディレクトリへ移動しても
  メディアが解決する。v3 プロジェクトが v4 として開き、絶対パスが保持される。
  `${PROJECT_ROOT}` を含む `Variable` パスが解決する。`resolved == None` の
  アセットを持つドキュメントで評価がパニックしない。
  ラウンドトリップ（保存 → ロード → 保存）でバイト一致。
- **単位 2**: 動画・静止画・連番のそれぞれで 1 フレーム取得できる
  （decoder をスタブした headless テスト）。静止画は同じフレームを 2 回
  評価してもデコードが 1 回だけ走る。`video` type_key の既存ドキュメントが
  読める。オフラインで透明フレームが返り、上位の合成が続く。
- **単位 3**: Import で `media_assets` が増え、Cmd+Z 1 回で消える。
  3 ファイル同時 D&D が 1 undo。probe 失敗ファイルが混ざっても
  成功分だけ入る。レイヤー化で `start_frame == 再生ヘッド` かつ
  `out_frame == ceil(duration × comp_fps)`。
- **単位 4**: 行が種別フィルタと検索で絞れる。選択が `MediaSelection` に出て
  Properties が追従する。使用中アセットの削除で確認が出る（参照数つき）。
  `Render` 内で I/O もグラフ走査もしない（`ravel-review` の render 純粋性）。
- **単位 5**: 同じアセットの 2 回目の表示でデコードが走らない。
  キャッシュディレクトリを消しても再生成される。生成失敗が繰り返し再試行に
  ならない。
- **単位 6**: オフラインアセットを Relink して評価が復活し、Cmd+Z で戻る。
  Properties でパス種別を切り替えて保存 → ロードで保持される。
- **単位 7**: `docs/ui-impl-status.md` の MediaBin 行が ✅ になり、
  REQ-UI-008 / REQ-UI-010 / REQ-PROJ-001 の該当受入条件がチェック済みになる。

## 検証

- `mise run check`。
- headless テストの置き場: アセットモデル・マイグレーションは `ravel-core` /
  `ravel-app` の `tests/`、行モデルとフィルタは `ravel-ui`、
  GPUI 経路は `crates/ravel-app/tests/`（`panels/mod.rs` に `mod tests` を置かない）。
- 実機（cliclick）: Finder から D&D → MediaBin に出る → ダブルクリックで
  レイヤー化 → Viewer に出る → 保存 → プロジェクトを別ディレクトリへ移動 →
  再オープンで解決している、の一連。

## 非対象

- プロキシ生成と切替（`ProxyInfo` は将来の予約として設計に残すだけ）。
- スマートコレクション、フォルダ階層、タグ（REQ-UI-008 の後半）。
- 一括再リンク・ディレクトリ探索・ハッシュによる同一性判定。
- 書き出し（エクスポート機能自体が未実装。`ravel-media` の `Encoder` に
  UI 経路が無く、`CommandId` に Export が無い）。
- 音声の再生・ミックス・解析（`docs/implementation/audio-plan.md`）。
- 画像シーケンスの EXR マルチパート / DPX タイムコード解釈。
