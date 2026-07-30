# 永続化を変更する

> 索引: [`README.md`](README.md)

`.ravprj` の構造とフォーマットバージョンの扱い。データモデルの定義は
[`../specifications/data-model.md`](../specifications/data-model.md)。

## `.ravprj` の中身

| エントリ | 内容 |
|---|---|
| `manifest.json` | `format_version` とプロジェクト情報。**マイグレーション連鎖の起点** |
| `document/main.ron` | Composition・レイヤー・ネットワーク（Subnet 入れ子含む）・キーフレーム・`media_assets`。決定的 RON |
| `settings.toml` | プロジェクト設定 |
| `ui_state.json` | UI 状態（アクティブコンポジション）。**任意エントリ**で、欠落時は `root_comp` にフォールバック |

保存時は前のリビジョンを `.bak` にする。書き込みはアトミック
（`CRIT-03` で修正済み）。

## 判断: バージョンを上げるか、上げないか

```text
既存フィールドの意味・型・単位を変える     → バージョンを上げてマイグレーションを書く
新しいフィールドを足すだけ                → 上げない。#[serde(default)] で読む
新しい任意エントリを足すだけ              → 上げない
```

**追加フィールドでバージョンを上げない**のは前例がある: `Layer.audio` は
format v4 のまま `#[serde(default)]` の追加フィールドとして入り、v5 も
マイグレーションも作っていない。`ui_state.json` も format_version 3 のまま
追加された。

この判断は `docs/implementation/roadmap.md` の基準 1（移行コストが時間で
増える単位を先に）にも影響する。**フィールド追加だけなら後回しのコストは
上がらない**ので、基準 1 を根拠に前倒しする理由にはならない。

## マイグレーションを書く

`crates/ravel-app/src/project/migration.rs`。`migrate_to_current` が
`format_version` を見て `migrate_vN_to_vN+1` を順に適用する連鎖。

- 1 段ずつ書く（v3 から v5 へ直接飛ばさない）
- 対象は `serde_json::Value`（型に落とす前）。型を変えている最中なので
  構造体経由では書けない
- **旧ファイルを 1 つでも読めなくしない。** 連鎖の各段にテストを置く
- ドキュメント本体（RON）の構造変更は、読み込み側で `#[serde(default)]` と
  variant 追加で吸収できるかを先に検討する

## ID の扱い

- ロード時に `NodeId` / `EdgeId` / `CompId` / `LayerId` のカウンタを
  ドキュメント最大 ID より先へ進める（REQ-LAYER-009）。新しい ID 種を足したら
  ここも足す
- `type_key` とパネルの `panel_id()` は**永続化に載る文字列**。後から変えると
  それぞれノードの復元とレイアウトの復元が壊れる

## メディア参照

パスは相対 / 変数形式で記録する（v4 で `assets/refs.json` を廃止）。
絶対パスは `Absolute` として読める。リンク切れはオフライン表示になり、
修復（Relink）は `MEDIA-7`。

## テスト

- ラウンドトリップ（保存 → 読込 → 等価）を必ず書く
- マイグレーションは**旧バージョンの実データ相当**を入力にする
- 追加フィールドは「欠落した入力が既定値で読める」ことを検査する
