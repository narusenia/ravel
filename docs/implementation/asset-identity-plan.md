# 素材の同一性（AssetId）実装計画

> **Status**: Planned — 2026-08-08

対象: `ravel-core` の `Document::media_assets` と参照側 3 系統、
`ravel-project` の永続化と移行、`ravel-app` の MediaBin とインポート。
関連要件は `REQ-PROJ-001`、`REQ-UI-008`、`REQ-UI-010`。

## 問題

### 素材の識別子がファイル名から作った文字列で、それがそのまま参照キー

`Document::media_assets` は `im::HashMap<String, MediaAssetEntry>`
（`composition/mod.rs:485`）。キーは `unique_asset_id`
（`ravel-app/src/project_state.rs:1508`）が作る:

```rust
let base = path.file_stem()…;          // "plate"
if !doc.media_assets.contains_key(&base) { return base; }
// 衝突したら "plate 2", "plate 3", …
```

この文字列を **3 系統が参照している**:

- `media` ノードの `asset_id` パラメータ（`registry/builtin.rs:513`）
- `AudioSource::asset_id`（`composition/mod.rs:131`）
- 露出パラメータの素材参照 `ASSET_REFERENCE_KEY`（`exposed/apply.rs:625`）

### 1. 削除して再インポートすると、古い参照が別のファイルに静かに繋がる

```
plate.mov をインポート        → asset_id = "plate"
MediaBin から削除             → キー "plate" が空く
別の plate.mov をインポート   → unique_asset_id() が再び "plate" を返す
                              → 古い media ノードの asset_id:"plate" が
                                 中身の違うファイルを指す
```

**エラーも警告も出ない。**「オフラインになった」なら気づけるが、
これは*繋がってしまう*ので気づけない。

### 2. 名前を変えられない

参照が名前そのものなので、`plate` を `背景プレート` に改名すると
参照が全部切れる。したがって改名機能を出せない。
`" 2"` という採番はインポート順に依存するので、どちらがどちらかも
プロジェクトを開いた人には分からない。

### 3. プロジェクト間でレイヤーをコピーすると意味が変わる

コピー元の `asset_id: "plate"` は、コピー先では別のファイルを指すか、
存在しない。

### CompId / LayerId には同じ問題が無い

`id.rs` の各 ID は `AtomicU64` の単調増加で、`advance_counter_past` が
ロード時にカウンタを最大値の先へ進める。**削除しても再利用されない。**
番号が飛ぶのは見た目の話で、参照が壊れる経路は無い。

## 決定事項

### 内部 ID と表示名を分ける。参照するのは内部 ID

`MediaAssetEntry` に不変の `AssetId` と、自由に変えられる `name` を持たせる。
3 系統の参照は `AssetId` を持つ。

これで 3 つの問題が同時に消える:

- 再インポートは新しい `AssetId` を採るので、**古い参照は繋がらない**
  （オフラインとして正しく見える）
- `name` は参照に使わないので**自由に改名できる**
- コピー元の `AssetId` はコピー先に無いので、**黙って別物を指さない**

### `CompId` / `LayerId` は触らない

上のとおり実害が無い。移行対象を増やす理由が無い。
**生の ID を UI に出したままでよい**（ユーザー判断）。

### `AssetId` の中身は「単調増加の整数」でよい。ULID にはしない

必要なのは*文書内で一意で、再利用されないこと*だけ。
`CompId` / `LayerId` が既に `AtomicU64` + `advance_counter_past` で
それを満たしており、**同じ仕組みを使えば新しい概念が増えない**。

ULID が効くのは「別々のマシンで作った ID をマージしても衝突しない」場面だが、
Ravel に分散編集は無く、`.ravprj` は 1 ファイル 1 文書。
プロジェクト間コピーは*衝突しない*ことより*繋がらない*ことが正しいので、
むしろ整数で十分。

### 移行は「ロード後の型付きパス」で行う

JSON を書き換える連鎖ではなく、読み込んだ `Document` に対して型付きで行う
（`v4 → v5` が実例）。旧文書では:

1. `media_assets` の各キー（文字列）に新しい `AssetId` を採番する
2. `name` には**元の文字列をそのまま**入れる（表示が変わらない）
3. 3 系統の参照を、元の文字列から採番した `AssetId` へ張り替える

旧文書の中では文字列キーが一意だったので、**この張り替えは常に成功する**。

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| AID-1 | `AssetId` 型と `MediaAssetEntry` の分離（フォーマット上げ + 移行） | — |
| AID-2 | 参照 3 系統の切り替え | AID-1 |
| AID-3 | インポートの採番と MediaBin の改名 UI | AID-2 |
| AID-4 | ロケール / 文書 | AID-1〜3 |

### 単位 1: `AssetId` 型と `MediaAssetEntry` の分離

- `id.rs` に `AssetId` を足す（`CompId` と同じ形。`next()` /
  `advance_counter_past()`）
- `MediaAssetEntry` に `name: String` を足し、`media_assets` のキーを
  `AssetId` にする
- `.ravprj` フォーマットを 1 つ上げる。**採番はマージ順** — 着手時に
  `manifest.rs` の `CURRENT_FORMAT_VERSION` を見て決める
  （`discrete-keyframes-plan.md` / `parameter-groups-plan.md` の `PGRP-4` /
  `CM-2` と競る）
- 移行を上記の 3 手順で書く

**完了条件**

- 旧 `.ravprj` が読め、素材の**表示名が変わらない**
- 移行後にラウンドトリップする
- ロード時に `AssetId` のカウンタが最大値の先へ進む（新規採番が衝突しない）
- 参照を持つ旧文書の移行テスト（`media` ノード / 音声 / 露出宣言の 3 系統すべて）

### 単位 2: 参照 3 系統の切り替え

- `media` ノードの `asset_id` パラメータ、`AudioSource::asset_id`、
  `ASSET_REFERENCE_KEY` を `AssetId` にする
- **`media` ノードのパラメータは `ParameterValue::String`** なので、
  綴りをどうするかをここで決める（`AssetId` の文字列表現を入れるか、
  パラメータの型を足すか）。露出パラメータからの素材差し替え（`EXPO-4`）が
  この値を書くので、そちらの経路も一緒に見る

**完了条件**

- 削除 → 同名ファイルの再インポートで、**古い参照がオフラインとして現れる**
  （別ファイルに繋がらない）ことのテスト
- 音声付き動画のレイヤーで、映像と音声が同じ素材を指し続ける
- 露出宣言からの素材差し替えが動く

### 単位 3: インポートの採番と MediaBin の改名 UI

- `unique_asset_id` を廃し、インポートは `AssetId::next()` を採る。
  `name` は `file_stem`、同名は表示上だけ `" 2"` を付ける（**参照には効かない**）
- MediaBin から `name` を改名できるようにする

**露出パラメータの所有権をここで決める（`AID-2` のレビューで出た）。**
`AID-2` は「宣言が作った素材」を `MediaAssetEntry::name` に入れた導出文字列
（`exposed:<宣言名>`）で見分けている。名前が一意でなくなると成立しないので、
**改名を出すこの単位が所有権の持ち方を決める**必要がある:

- ユーザーが素材を `exposed:foo` に改名すると、宣言がその素材を自分のものと
  誤認して**パスを上書きできる**
- 同名の素材が 2 つあると `Document::media_asset_id_by_name` が `None` を返し、
  同じ値を適用するたびに**新しい素材が増える**（冪等性が崩れる）

どちらも今日は到達しない（名前はファイルステムで、`exposed:` 接頭辞は
インポートが付けない）。**改名を出した瞬間に到達する**ので、名前ではなく
宣言 → `AssetId` の対応を持つ形に変えるのがこの単位の仕事。

**採った形**（実装済み）: `MediaAssetEntry` に `exposed_owner: Option<String>`
（宣言名）を足し、所有物の判定はこのフィールドだけで行う。名前は表示ラベルに
降格し、`apply` は再適用でも**ユーザーが付けた名前を書き換えない**。同じ宣言を
名乗るエントリが 2 つある文書（手編集のみ到達）は `AssetOwnerAmbiguous` で拒否。
v8 → v9 の移行は旧来の `exposed:<宣言名>` という名前からこのフィールドを埋める
（旧文書では名前が唯一の記録なので、そこだけは名前が正）。`#[serde(default)]`
での追加なので `.ravprj` のフォーマット版は上げていない。`unique_asset_id` は
`unique_display_name` に改名して残した（採番は表示名だけの話）。

**完了条件**

- 同名ファイルを 2 つインポートして、両方が別の素材として扱われる
- 改名しても参照が切れない
- 改名が 1 undo
- **改名で露出宣言の所有関係が壊れない**（`exposed:` を名乗る素材を作っても
  宣言がそれを乗っ取らない）

### 単位 4: ロケール / 文書

**完了条件**

- `docs/specifications/data-model.md` の素材の記述が追随
- `docs/ui-impl-status.md` の MediaBin 行が追随

## 非対象

- **`CompId` / `LayerId` の変更**。実害が無く、移行対象を増やすだけ
- **ULID / UUID の採用**。上記のとおり分散編集が無いので過剰
- **内容ハッシュによる同一性**（同じ中身のファイルを 1 つの素材と見なす）。
  再リンクの話（`MEDIA-6`）と絡むので、そちらで判断する
- **プロジェクト間の素材マージ**。コピー時に「繋がらない」ことを正とし、
  再リンクはユーザーの操作に任せる
