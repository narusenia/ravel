# 実装バックログ

全ライブ計画の実装単位を 1 枚に並べたもの。**着手できるものを探すための
ファイル**で、設計の正は各計画書にある。

- 単位の内容・完了条件は計画書を見る。ここには要約しか書かない。
- 計画書を更新したらこの表も更新する。片方だけ直さない。
- 完了した単位は行を消さず `✅` にして PR 番号を入れる。

最終更新: 2026-07-27

## 凡例

| 記号 | 意味 |
|---|---|
| ✅ | マージ済み |
| 🟡 | 着手可能（依存が解決済み） |
| ⬜ | 依存待ち |
| ❓ | 前提条件の判断待ち（測定・設計決着など） |

## 今すぐ着手できるもの

依存が無いか、依存がすべて解決している単位。

| ID | 単位 | 計画 |
|---|---|---|
| SCOPE-1 | `PathSegment` のスコープ次元（挙動不変） | `evaluation-scope-plan.md` |
| MOD-1 | 変調の合成モード（`CombineMode`）と成分マスク | `per-instance-modulation-plan.md` |
| PANEL-1 | 実効レイアウトの分離（挙動不変のリファクタ） | `panel-placement-plan.md` |
| OPS-1 | `geometry.blast`（要素削除） | `geometry-ops-plan.md` |
| OPS-2 | `geometry.sort`（並べ替え） | `geometry-ops-plan.md` |
| OPS-3 | `geometry.resample` | `geometry-ops-plan.md` |
| OPS-4 | `geometry.measure` | `geometry-ops-plan.md` |
| OPS-5 | `geometry.switch` / `geometry.null` | `geometry-ops-plan.md` |
| FX-1 | カラー調整とカラーグレーディング | `effects-library-plan.md` |
| FX-2 | ブラー / シャープ / ディストーション | `effects-library-plan.md` |
| FX-3 | 生成とスタイライズ | `effects-library-plan.md` |
| FX-4 | トランスフォーム拡張と合成（マスク / キーイング） | `effects-library-plan.md` |
| MEDIA-6 | メディア Properties + 再リンク | `media-import-plan.md` |
| AUDIO-5 | 波形表示 | `audio-plan.md` |
| AUDIO-6 | 解析ノード（RMS / ピーク。**FFT クレート追加は禁止**） | `audio-plan.md` |

FX-1〜4 と OPS-1〜5 は互いに独立で、並列委譲しやすい。

**SCOPE-1 を SIM-1 より先に通すこと。** SIM / FX-5 / グラフ内反復が
同じキャッシュ制約に当たっており、軸を共通化しないと機構が 3 つに分裂する
（`evaluation-scope-plan.md` の「なぜ今か」）。

## 全単位

### 評価スコープ軸とグラフ内反復（REQ-CORE-013）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| SCOPE-1 | 🟡 | `PathSegment` のスコープ次元（挙動不変） | — |
| SCOPE-2 | ⬜ | 時間シフト経路（FX-5 の土台） | SCOPE-1 |
| SCOPE-3 | ⬜ | `geometry.iterate`（ピース単位反復） | SCOPE-1 |
| SCOPE-4 | ⬜ | 要素スコープ（group）規約の適用 | SCOPE-3 |
| SCOPE-5 | ⬜ | 文書更新 | SCOPE-4 |

### ジオメトリ操作ノード拡充

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| OPS-1 | 🟡 | `geometry.blast`（要素削除） | — |
| OPS-2 | 🟡 | `geometry.sort`（並べ替え） | — |
| OPS-3 | 🟡 | `geometry.resample` | — |
| OPS-4 | 🟡 | `geometry.measure` | — |
| OPS-5 | 🟡 | `geometry.switch` / `geometry.null` | — |
| OPS-6 | ⬜ | レジストリ / ロケール / 文書 | OPS-1〜5 |

OPS-1（削除）と OPS-2（並べ替え）は SCOPE-4 の group 規約と対になる。
group で絞れても消せない・並べ替えられないと group は半端に終わる。
OPS-2 は MOD-3 の stagger の順序を決めるので実質セット。

### per-instance 変調（REQ-MOGRAPH-001 残件）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| MOD-1 | 🟡 | 合成モードと成分マスク | — |
| MOD-2 | ⬜ | `FieldSample` 構造体化 + `field.attribute` | MOD-1 |
| MOD-3 | ⬜ | 駆動ソース `field.time` / `field.constant` | MOD-2 |
| MOD-4 | ⬜ | `attribute.delete`（属性**列**の削除。要素削除は OPS-1） | — |
| MOD-5 | ⬜ | ゴールデン検証と文書更新 | MOD-1〜4 |

### パネル配置（#181）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PANEL-1 | 🟡 | 実効レイアウトの分離 | — |
| PANEL-2 | ⬜ | 既定ドックスロットと挿入 / 削除 | PANEL-1 |
| PANEL-3 | ⬜ | 実機確認と文書更新 | PANEL-2 |

### 属性スプレッドシート

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| SHEET-1 | 🟡 | `EvalRequest` の複数ターゲット化 | — |
| SHEET-2 | ⬜ | 選択ノード評価と `SelectedGeometry` グローバル | SHEET-1 |
| SHEET-3 | ⬜ | パネル本体（`DataTable`） | SHEET-2, PANEL-2 |
| SHEET-4 | ⬜ | 実機確認と文書更新 | SHEET-3 |

SHEET-1 と SIM-3 は同じ型（`EvalRequest` / `EvalUpdate`）を触る。
**実施順を決めてから着手する。**

### タイポグラフィ（REQ-MOGRAPH-004）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| TYPE-1 | ⬜ | フォント解決 | — |
| TYPE-2 | ⬜ | シェーピングとレイアウト → インスタンスジオメトリ | TYPE-1 |
| TYPE-3 | ⬜ | レイヤーテンプレートと Properties | TYPE-2 |
| TYPE-4 | ⬜ | パス沿い配置 | TYPE-2 |
| TYPE-5 | ⬜ | `text.to_path` とフィールド被変調 | TYPE-2, MOD-5 |
| TYPE-6 | ⬜ | 縦書きと禁則処理 | TYPE-2 |
| TYPE-7 | ⬜ | ノードプリセットと文書更新 | TYPE-3, TYPE-5, MOD-5 |

TYPE-1 は依存が無いので先行できるが、計画全体は MOD-5 完了が前提。

### ステートフル評価（REQ-CORE-011）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| SIM-1 | ⬜ | `StatefulProcessor` と sim キャッシュの骨格 | SCOPE-1 |
| SIM-2 | ⬜ | 無効化 | SIM-1 |
| SIM-3 | ⬜ | 暫定表示とバックグラウンド充填 | SIM-2 |
| SIM-4 | ⬜ | 文書更新 | SIM-3 |

### パーティクル（REQ-MOGRAPH-002）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PART-1 | ⬜ | `particle.simulate` の骨格 | SIM-3 |
| PART-2 | ⬜ | エミッタジオメトリからの発生 | PART-1 |
| PART-3 | ⬜ | フィールドフォース | PART-1, MOD-3 |
| PART-4 | ⬜ | スクラブ耐性と他ノードへの流用 | PART-2, PART-3 |
| PART-5 | ⬜ | 文書更新 | PART-4 |
| PART-6 | ❓ | GPU シミュレーション | PART-5, GPU-3, **VRAM キャッシュ方式の決着** |

### エフェクトライブラリ（REQ-MOGRAPH-005）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| FX-1 | 🟡 | カラー調整とカラーグレーディング | — |
| FX-2 | 🟡 | ブラー / シャープ / ディストーション | — |
| FX-3 | 🟡 | 生成とスタイライズ | — |
| FX-4 | 🟡 | トランスフォーム拡張と合成 | — |
| FX-5 | ⬜ | 時間系（`SCOPE-2` の時間シフト経路に載る） | FX-1〜4, SCOPE-2 |
| FX-6 | ⬜ | レジストリ / ロケール / 文書 | FX-1〜5 |

### GPU 常駐ジオメトリ

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| GPU-0 | ❓ | **Phase 0 測定**（実施 / 縮小 / 中止の判断） | MOD-5 |
| GPU-1 | ❓ | `GpuGeometry` 型と転送 | GPU-0 = 実施 |
| GPU-2 | ❓ | フィールドの WGSL 評価 | GPU-1 |
| GPU-3 | ❓ | 生成ノードの GPU 化 | GPU-2 |
| GPU-4 | ❓ | 文書更新 | GPU-3 |

GPU-0 は**測定で中止しうる**。既存の 0.007 ms（`perf-baseline.md`
シナリオ c）は**キャッシュ温**の数字で、未キャッシュのフィールド評価は
未測定。GPU-0 はそこを測るのが目的。

### メディア / オーディオ（進行中）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| MEDIA-1〜5 | ✅ | アセットモデル / media ノード / インポート / MediaBin / サムネイル | #167, #173, #176, #177, #169 |
| MEDIA-6 | 🟡 | Properties + 再リンク | — |
| MEDIA-7 | ⬜ | オフライン表示 + 文書 | MEDIA-6 |
| AUDIO-1〜4 | ✅ | データモデル / ミキサ / 再生配線 / 動画音声 | #172, #168, #174, #178 |
| AUDIO-5 | 🟡 | 波形表示 | — |
| AUDIO-6 | 🟡 | 解析ノード（RMS / ピーク） | — |
| AUDIO-7 | ⬜ | バンクのタグ・試聴 | AUDIO-5 |

### 3D（スケッチ）

`3d-basics-sketch.md` は実装単位が未確定。TYPE-7 と GPU-3 の完了後に
詳細を埋める。

## 計画外の課題

| 項目 | 内容 |
|---|---|
| #181 | View トグルがプリセット配置依存 → `panel-placement-plan.md` で対応 |
| グローバル設定層の配線 | `settings.rs` の 4 層マージと TOML 入出力は実装済みだが、global 層が `global_settings_path()` から読み書きされていない（`resolved_settings` の呼び出し元がテストのみ）。レイアウト永続化の前提。`panel-placement-plan.md` の非対象 |
| `decode_audio_chunk` のシーク単位 | #179 は映像側のみ修正。音声側に `AV_TIME_BASE` 単位の問題が残る可能性（`start_sample = 0` 分岐で現状は表面化せず） |
| `decode_full_audio` の確保量 | 常に 128MB 相当の `Vec::with_capacity` |
| 実コーデック音声テスト | `ffmpeg` feature が既定オフのため **CI で走らない** |
| Lua / 式 | REQ-CODE-001 / REQ-PLUGIN-003。REQ-MOGRAPH-001 と REQ-CORE-010 の受入条件が 1 つずつこれ待ちで残る |
| トランジション | REQ-MOGRAPH-005 の受入条件だがタイムライン側の仕事。計画なし |

## 要件の残り（Must のみ）

| 要件 | 未達の受入条件 | 担当計画 |
|---|---|---|
| REQ-CORE-010 | 属性の削除 / Lua 式からの参照 | MOD-4 / なし |
| REQ-CORE-011 | 全部 | `stateful-eval-plan.md` |
| REQ-MOGRAPH-001 | フィールド変調 / 変調結果 / Lua 式 | MOD-1〜5 / なし |
| REQ-MOGRAPH-004 | 全部 | `typography-plan.md` |
| REQ-MOGRAPH-005 | ほぼ全部 | `effects-library-plan.md`（トランジション除く） |

Should 以下で状態が変わったもの:

| 要件 | 変更 |
|---|---|
| REQ-CORE-013 | Could → **Should**。「v1 では採用しない」を撤回（`evaluation-scope-plan.md`） |
