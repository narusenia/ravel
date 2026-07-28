# 実装バックログ

全ライブ計画の実装単位を 1 枚に並べたもの。**着手できるものを探すための
ファイル**で、設計の正は各計画書にある。

- 単位の内容・完了条件は計画書を見る。ここには要約しか書かない。
- 計画書を更新したらこの表も更新する。片方だけ直さない。
- 完了した単位は行を消さず `✅` にして PR 番号を入れる。

最終更新: 2026-07-28

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
| GPUCOMP-7 | リードバック回数と CPU/GPU 一致の回帰テスト | `gpu-compositing-plan.md` |
| SCOPE-2 | 時間シフト経路（FX-5 の土台） | `evaluation-scope-plan.md` |
| SCOPE-3 | `geometry.iterate`（ピース単位反復） | `evaluation-scope-plan.md` |
| SIM-1 | `StatefulProcessor` と sim キャッシュの骨格 | `stateful-eval-plan.md` |
| MOD-3 | 駆動ソース `field.time` / `field.constant` | `per-instance-modulation-plan.md` |
| MOD-4 | `attribute.delete`（属性列の削除） | `per-instance-modulation-plan.md` |
| VEC-1 | 二項合成の多相化 | `vector-field-plan.md` |
| PANEL-1 | 実効レイアウトの分離（挙動不変のリファクタ） | `panel-placement-plan.md` |
| OPS-1 | `geometry.blast`（要素削除） | `geometry-ops-plan.md` |
| OPS-2 | `geometry.sort`（並べ替え） | `geometry-ops-plan.md` |
| OPS-3 | `geometry.resample` | `geometry-ops-plan.md` |
| OPS-4 | `geometry.measure` | `geometry-ops-plan.md` |
| OPS-5 | `geometry.switch` / `geometry.null` | `geometry-ops-plan.md` |
| OPS-6 | `geometry.group_index`（index で要素指定） | `geometry-ops-plan.md` |
| OPS-7 | `geometry.repeat`（トランスフォームリピータ） | `geometry-ops-plan.md` |
| OPS-8 | デフォーマ（bend / twist / taper） | `geometry-ops-plan.md` |
| STYLE-1 | 塗り・線のスタイル属性読み出し | `style-attributes-plan.md` |
| SHELL-1 | `time_remap` の配線 | `layer-shell-wiring-plan.md` |
| SHELL-2 | `track_matte` の配線 | `layer-shell-wiring-plan.md` |
| BLUR-2 | キャッシュ有効性を `time` 基準へ | `motion-blur-plan.md` |
| PATH-0 | ブーリアンの実装方針評価（依存判断） | `path-ops-plan.md` |
| EXPORT-1 | エンコーダ抽象と実行時列挙 | `render-export-plan.md` |
| FX-1 | カラー調整とカラーグレーディング | `effects-library-plan.md` |
| FX-2 | ブラー / シャープ / ディストーション | `effects-library-plan.md` |
| FX-3 | 生成とスタイライズ | `effects-library-plan.md` |
| FX-4 | トランスフォーム拡張と合成（マスク / キーイング） | `effects-library-plan.md` |
| MEDIA-6 | メディア Properties + 再リンク | `media-import-plan.md` |
| AUDIO-5 | 波形表示 | `audio-plan.md` |
| AUDIO-6 | 解析ノード（RMS / ピーク。**FFT クレート追加は禁止**） | `audio-plan.md` |

FX-1〜4 と OPS-1〜5 は互いに独立で、並列委譲しやすい。

SCOPE-1（#186）が入ったので、SIM / FX-5 / グラフ内反復が共有する軸は
確定した。SIM-1 は `SimTrack` が `NodeKey` を共有する前提で書くこと
（`evaluation-scope-plan.md` の「sim キャッシュだけは別扱いを残す」）。

## 全単位

### UI 応答性（`issues/README.md` 第1段）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| RESP-1 | ✅ | 評価結果到着をパネル notify から切り離す（CRIT-01） | #191 |
| RESP-2 | ✅ | ドキュメント世代でパネル再構築をゲートする（HIGH-07） | #192 |
| RESP-3 | ✅ | パラメータ編集で GPU パイプラインを再コンパイルしない（HIGH-06） | #193 |

第1段は完了（`done/ui-responsiveness-plan.md`）。ただし**実測では体感の主因は
第2段**だった — HIGH-05（シェル合成の CPU per-pixel）と HIGH-04（リードバック）。
第2段は `gpu-compositing-plan.md` に降りている（下表）。第3段以降と、
保留した MED-UI-06（同じ変更が2経路で届く重複 sync）は `issues/` 側に残っている。

### GPU 合成パイプライン（`issues/README.md` 第2段）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| GPUCOMP-1 | ✅ | `perf_baseline` に N レイヤーのシェル合成シナリオを追加 | #197 |
| GPUCOMP-2 | ✅ #198 | `comp.opacity` の GPU 版 | GPUCOMP-1 |
| GPUCOMP-3 | ✅ #198 | `comp.transform` の GPU 版 + アルファ規約・タップ境界の是正 | GPUCOMP-2 |
| GPUCOMP-4 | ✅ #198 | `blur.wgsl` のアルファ規約統一（MED-GPU-02 の残り） | GPUCOMP-3 |
| GPUCOMP-5 | ✅ | `comp.merge.*`（5モード）の GPU 版 | GPUCOMP-3 |
| GPUCOMP-6 | ✅ | `comp.merge.adjustment` の GPU 版 | GPUCOMP-5 |
| GPUCOMP-7 | 🟡 | リードバック回数と CPU/GPU 一致の回帰テスト | GPUCOMP-6 |
| GPUCOMP-8 | ⬜ | リードバック実装の改善（HIGH-04） | GPUCOMP-7 |
| GPUCOMP-9 | ⬜ | f32→BGRA 変換を評価ワーカーへ（HIGH-08 / HIGH-09） | GPUCOMP-8 |
| GPUCOMP-10 | ❓ | 非同期リードバック（測定ゲート） | GPUCOMP-9 |
| GPUCOMP-11 | ❓ | `VIEWER_MAX_DIM` 引き上げ / ゼロコピー表示の判断（測定ゲート） | GPUCOMP-9 |

GPUCOMP-1（#197）で測定の土台が入り、readback が **N 回 / 完成評価**であることを
実測で確認した。10 レイヤー再生形では `comp.transform` が `evaluate` の 78% で、
その内訳は readback 約 2.2 ms + CPU per-pixel ループ約 1.1 ms（評価1回・1レイヤーあたり）。
数字は `perf-baseline.md`「GPU シェル合成チェーン baseline」。

GPUCOMP-2 / 3 / 4 で transform / opacity が GPU 経路に移り、`comp.transform` は
3.32 ms/回 → 0.067 ms/回、`evaluate` 合計は −14%。ただし **readback の回数は
10 / 完成評価のまま**で、発生位置が transform から merge の `ensure_cpu` に移っただけだった。
数字は `perf-baseline.md`「GPU シェル transform / opacity 投入後」。

GPUCOMP-5 / 6 で merge も GPU 化し、**シェルチェーン由来の readback が 0 になった**
（10 レイヤー再生形で 10 → 0、`evaluate` 合計 −94%、`comp.merge.*` は 1.7〜5.3 ms/回 →
0.02〜0.05 ms/回）。残る1回はアプリ側 `GpuEvalHooks::finalize` の表示用で、
「完成評価あたり 1」の pin は GPUCOMP-7 で入れる。
数字は `perf-baseline.md`「GPU シェル merge 投入後」。

### 評価スコープ軸とグラフ内反復（REQ-CORE-013）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| SCOPE-1 | ✅ | `PathSegment` のスコープ次元（挙動不変） | #186 |
| SCOPE-2 | 🟡 | 時間シフト経路（FX-5 の土台） | SCOPE-1 |
| SCOPE-3 | 🟡 | `geometry.iterate`（ピース単位反復） | SCOPE-1 |
| SCOPE-4 | ⬜ | 要素スコープ（group）規約の適用（`field.apply` は MOD-1 が担当） | SCOPE-3 |
| SCOPE-5 | ⬜ | 文書更新 | SCOPE-4 |

### ジオメトリ操作ノード拡充

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| OPS-1 | 🟡 | `geometry.blast`（要素削除） | — |
| OPS-2 | 🟡 | `geometry.sort`（並べ替え） | — |
| OPS-3 | 🟡 | `geometry.resample` | — |
| OPS-4 | 🟡 | `geometry.measure`（bounds / size 含む） | — |
| OPS-5 | 🟡 | `geometry.switch` / `geometry.null` | — |
| OPS-6 | 🟡 | `geometry.group_index`（index で要素指定） | — |
| OPS-7 | 🟡 | `geometry.repeat`（トランスフォームリピータ） | — |
| OPS-8 | 🟡 | デフォーマ（bend / twist / taper） | — |
| OPS-9 | ⬜ | `geometry.distribute`（要素サイズ考慮の分布） | OPS-4 |
| OPS-10 | ⬜ | レジストリ / ロケール / 文書 | OPS-1〜9 |

### 塗り・線のスタイル属性化

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| STYLE-1 | 🟡 | スタイル属性の読み出し（CPU / GPU） | — |
| STYLE-2 | ⬜ | `style.fill` / `style.stroke` ノード | STYLE-1 |
| STYLE-3 | ⬜ | ダッシュ・キャップ・ジョイン | STYLE-1 |
| STYLE-4 | ⬜ | 変調との結合検証と文書 | STYLE-2, MOD-1 |

### ベクタ場

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| VEC-1 | 🟡 | 二項合成の多相化 | MOD-2 |
| VEC-2 | ⬜ | 変換ノード（length / component / compose / angle） | VEC-1 |
| VEC-3 | ⬜ | ベクタ場（direction_to / curl_noise / gradient / radial） | VEC-2 |
| VEC-4 | ⬜ | look-at・フロー場のゴールデン検証と文書 | VEC-3 |

### パス操作

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PATH-0 | 🟡 | **ブーリアンの実装方針評価**（依存追加の可否含む） | — |
| PATH-1 | ❓ | `path.boolean` | PATH-0 = クレート採用 |
| PATH-2 | ⬜ | `path.offset` | — |
| PATH-3 | ⬜ | `path.round_corners` | — |
| PATH-4 | ⬜ | `path.simplify` | — |
| PATH-5 | ⬜ | `path.trim` | OPS-3 |
| PATH-6 | ⬜ | レジストリ / ロケール / 文書 | PATH-1〜5 |

### レイヤー殻の未配線フィールド

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| SHELL-1 | 🟡 | `time_remap` の配線 | （BLUR-1 完了済みなので分数時刻が正しく出る） |
| SHELL-2 | 🟡 | `track_matte` の配線 | — |
| SHELL-3 | ⬜ | UI 露出 | SHELL-1, SHELL-2 |
| SHELL-4 | ⬜ | 文書更新 | SHELL-3 |

### モーションブラー（REQ-RENDER-004）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| BLUR-1 | ✅ | アニメーションチャネルの連続時間化 | #187 |
| BLUR-2 | 🟡 | **キャッシュ有効性を `time` 基準へ** | BLUR-1 |
| BLUR-3 | ⬜ | 品質段階 `EvalContext.quality` | BLUR-2 |
| BLUR-4 | ⬜ | `comp.motion_blur` と殻フィールド | BLUR-3 |
| BLUR-5 | ⬜ | 文書更新 | BLUR-4 |

BLUR-2 を飛ばすと**「実装したのにブレない」形で静かに壊れる**
（キャッシュが整数 frame を見ているため 2 サンプル目以降がヒットする）。

### 書き出し（REQ-RENDER-001）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| EXPORT-1 | 🟡 | エンコーダ抽象と実行時列挙 | — |
| EXPORT-2 | ⬜ | レンダーワーカーとキュー | EXPORT-1, BLUR-3 |
| EXPORT-3 | ⬜ | 書き出し UI | EXPORT-2, PANEL-2 |
| EXPORT-4 | ⬜ | 文書更新 | EXPORT-3 |

### Align パネル

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| ALIGN-1 | 🟡 | 整列・分布の計算（ヘッドレス） | — |
| ALIGN-2 | ⬜ | パネルと配線 | ALIGN-1, PANEL-2 |
| ALIGN-3 | ⬜ | 文書更新 | ALIGN-2 |

OPS-1（削除）と OPS-2（並べ替え）は group 規約と対になる。
group で絞れても消せない・並べ替えられないと group は半端に終わる。

**OPS-2 は MOD-5 より前に通すこと。** `index` は生成順固定なので、OPS-2 が
無いと stagger は「行優先で順に」しか出せず、MOD-5 のゴールデンテストが
stagger の実用性を示せない。OPS-2 は依存が無く MOD とも独立なので
MOD-1〜3 と並行できる。

### per-instance 変調（REQ-MOGRAPH-001 残件）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| MOD-1 | ✅ | 合成モードと成分マスクと `group` | #188 |
| MOD-2 | ✅ | `FieldSample` 構造体化 + `field.attribute` | #189 |
| MOD-3 | 🟡 | 駆動ソース `field.time` / `field.constant` | MOD-2 |
| MOD-4 | 🟡 | `attribute.delete`（属性**列**の削除。要素削除は OPS-1） | — |
| MOD-5 | ⬜ | ゴールデン検証と文書更新 | MOD-1〜4, OPS-2 |

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
| SIM-1 | 🟡 | `StatefulProcessor` と sim キャッシュの骨格 | SCOPE-1 |
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
| REQ-DATA 全体 | CSV/JSON → Table → 属性バインディング。データ駆動インフォグラフィックスの柱ごと欠落。計画なし |
| REQ-RENDER-002 Write ノード | 評価純粋性とディスクキャッシュ設計の問題。`render-export-plan.md` の非対象 |
| REQ-RENDER-003 OCIO | カラーマネジメント。計画なし |
| Fuse / パス自己交差解消 | 空間分割構造が要る |
| ビート検出 | FFT 見送りの延長 |
| レイヤー制約（look-at / パス追従） | ジオメトリ側は VEC-3 で解決するが、レイヤー殻には無い |

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
