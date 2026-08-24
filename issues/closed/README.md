# closed — 解決済みの課題

解決済みの個票を集めた場所。**未解決分の索引は [`../README.md`](../README.md)** で、
そちらは未解決だけを扱う。

個票は起票時の記述をそのまま残し、各項目の `**解決済み**` 行が結果と PR 番号 /
フェーズを記録する。削除しないのは、同じ症状が再発したときに「前回どう直したか」を
引けるようにするため。

解決の根拠は `docs/implementation/roadmap.md` のフェーズ（`実施結果` 節）と、
各実装計画書（`docs/implementation/done/`）にある。

| 深刻度 | 件数 | 形式 |
|---|---|---|
| critical | 4 | 1件1ファイル |
| high | 33 | 1件1ファイル |
| medium | 46 | 領域別5ファイル（`medium-*.md`） |
| low | 10 | [low.md](low.md) 1ファイル |

## critical（4件 — 起票分すべて解決）

| ID | 内容 | 解決 |
|---|---|---|
| [CRIT-01](CRIT-01-eval-update-notifies-whole-workspace.md) | 評価結果ごとに全5パネルが再構築 | 第1段 / PR #191-193 |
| [CRIT-02](CRIT-02-save-failure-invisible-and-swallows-quit.md) | 保存失敗が不可視、Quit / Close を無言で破棄 | フェーズ A2 |
| [CRIT-03](CRIT-03-project-write-not-atomic.md) | 保存が非アトミック、`.bak` フォールバックなし | フェーズ A2 |
| [CRIT-04](CRIT-04-uncommitted-gesture-baked-by-foreign-commit.md) | 未コミットジェスチャーが他パネルのコミットで焼き付き | フェーズ A2 |

## high（33件）

| ID | 内容 | 解決 |
|---|---|---|
| [HIGH-01](HIGH-01-evaluator-no-adjacency-index.md) | 評価器に隣接インデックスが無く 1 回の pull が O(N·E) | `RESP3-1`（#395） |
| [HIGH-02](HIGH-02-graph-eq-no-ptr-eq-fastpath.md) | 編集ごとに全レイヤーネットワークを deep compare | `RESP3-2`（#395） |
| [HIGH-03](HIGH-03-params-resolved-per-visit.md) | キャッシュヒット時もパラメータ全再解決 | `CACHE-2` |
| [HIGH-04](HIGH-04-per-frame-blocking-readback.md) | 表示フレームごとにブロッキングリードバック（毎回ステージング確保 + デバイス全体待ち + 二重コピー） | `GPUBK-6` |
| [HIGH-05](HIGH-05-shell-chain-cpu-per-pixel.md) | 殻の合成チェーンが CPU per-pixel | `GPUCOMP-2/3/5/6` |
| [HIGH-06](HIGH-06-pipeline-recompiled-per-param-edit.md) | パラメータ編集ごとにパイプライン再コンパイル | 第1段 |
| [HIGH-07](HIGH-07-document-changed-cascade-per-mouse-move.md) | マウス移動ごとに `document_changed` 全カスケード | 第1段 |
| [HIGH-08](HIGH-08-ui-thread-f32-to-bgra-conversion.md) | UI スレッドで全フレーム f32→BGRA 変換 | `GPUCOMP-9` |
| [HIGH-09](HIGH-09-viewer-gpu-cpu-gpu-roundtrip.md) | ビューアが毎フレーム GPU→CPU→GPU 往復 | `ZC-1`〜`ZC-8`（#391） |
| [HIGH-10](HIGH-10-audio-chunk-seek-wrong-time-base.md) | 音声チャンク seek の時間基準誤り | フェーズ A3 |
| [HIGH-11](HIGH-11-audio-chunk-no-trim.md) | 音声チャンクが要求位置までトリムされない | フェーズ A3 |
| [HIGH-12](HIGH-12-pause-does-not-stop-queued-audio.md) | Pause でキュー済み音声が止まらない | フェーズ A3（epoch） |
| [HIGH-13](HIGH-13-seek-does-not-flush-audio-queue.md) | Seek でチャンクキューを flush しない | フェーズ A3（epoch） |
| [HIGH-14](HIGH-14-clock-advances-over-underrun.md) | アンダーラン中も同期クロックが進む | フェーズ A3（epoch） |
| [HIGH-15](HIGH-15-settrack-resamples-on-prep-thread.md) | `SetTrack` が prep スレッドで全長リサンプル | フェーズ A3 / A4 |
| [HIGH-16](HIGH-16-no-decoded-frame-cache.md) | デコード済みフレームキャッシュが無く逆方向スクラブが GOP を再デコード | `CACHE-8` |
| [HIGH-17](HIGH-17-sws-scaler-recreated-per-frame.md) | sws スケーラをフレームごとに再生成 + スカラー per-pixel 変換 | #380 |
| [HIGH-18](HIGH-18-open-failure-invisible.md) | プロジェクトを開けないが無言 | フェーズ A2 |
| [HIGH-20](HIGH-20-media-import-failure-invisible.md) | メディアインポート失敗が無言 | フェーズ A2 |
| [HIGH-21](HIGH-21-node-editor-repaints-every-playback-frame.md) | NodeEditor が再生中毎フレーム全再構築 | フェーズ A |
| [HIGH-22](HIGH-22-port-hit-test-ignores-z-order.md) | ポートヒットテストが z 順を無視 | フェーズ A |
| [HIGH-23](HIGH-23-resampled-audio-not-cached.md) | リサンプル結果が未キャッシュ | フェーズ A4 |
| [HIGH-24](HIGH-24-timeline-end-pause-not-forwarded-to-audio.md) | 終端の自動 Pause が音声へ転送されない | フェーズ A4 |
| [HIGH-25](HIGH-25-compositing-in-display-referred-space.md) | 合成が display-referred 空間で行われている | フェーズ CM |
| [HIGH-28](HIGH-28-scrub-commit-lost-when-properties-rebuilds.md) | スクラブの `Commit` が再構築で失われ undo が飛ぶ | 実機フィードバック |
| [HIGH-27](HIGH-27-timeline-keyframes-invisible-inside-subnets.md) | Subnet の中のキーフレームが Timeline から消える | 実機フィードバック |
| [HIGH-30](HIGH-30-subnet-port-rename-drops-outer-edges.md) | Subnet 内のポート名変更で外側のエッジが消える | 実機フィードバック |
| [HIGH-31](HIGH-31-float-decode-through-8bit-rgba.md) | float / 高ビット深度のデコードが 8bit RGBA を経由 | 画素形式別の取り込み経路（float 直読み / RGBA64） |
| [HIGH-32](HIGH-32-linear-ingest-powf-per-pixel.md) | 線形 ingest が画素ごとに f64 の transfer function を評価し、デコードが 1 フレーム数十 ms に落ちる | #378 |
| [HIGH-29](HIGH-29-no-menu-bar-outside-macos.md) | Windows / Linux にメニューが 1 つも出ない | 実機フィードバック（Linux は未確認） |
| [HIGH-26](HIGH-26-ravprj-saves-deeper-than-it-loads.md) | 保存できた `.ravprj` が二度と開けない（保存側と読み込み側で上限が違う） | 上限の一本化（`MAX_SUBNET_DEPTH` 16 + `RON_RECURSION_LIMIT`） |
| [HIGH-34](HIGH-34-offline-media-renders-silently-transparent.md) | オフラインの素材がレンダー結果を黙って透明にする（機械可読な警告が無い） | `WARN-2`（`media-offline` / `media-unreadable`） |
| [HIGH-35](HIGH-35-identifier-parameters-can-be-driven-by-a-wire.md) | 参照 ID のパラメータをフレームごとに動かせ、ID の予約が外れる | `WARN-1`（読み口を `ParameterValue::identifier` へ集約） |

## medium（46件）

- [medium-app-shell.md](medium-app-shell.md) — `MED-APP-01` `12` `16` `18` `21` `22` `23` `24` `25` `26` `27` `28` `30` `31` `32` `33` `34` `35` `36`
- [medium-core-evaluator.md](medium-core-evaluator.md) — `MED-CORE-01` `02` `03` `05` `06` `07` `09` `10`
  （`MED-CORE-04` は 2026-08-03 に再判定して未解決へ戻した。デシリアライズ経路は
  `HIGH-26` で閉じたが、評価側の再帰が残るので未解決のまま。
  [`../medium/core-evaluator.md`](../medium/core-evaluator.md)）
- [medium-gpu-nodes.md](medium-gpu-nodes.md) — `MED-GPU-01` `02` `03` `04` `05` `07`
- [medium-ui-rendering.md](medium-ui-rendering.md) — `MED-UI-01` `02` `03` `04` `05` `06`
  （未解決分は `MED-UI-07` のみ。
  [`../medium/ui-rendering.md`](../medium/ui-rendering.md)）
- [medium-media-audio.md](medium-media-audio.md) — `MED-MED-03` `04` `05` `07` / `MED-AUD-01` `02` `03`

## low（10件）

[low.md](low.md) — `LOW-GPU-01` / `LOW-AUD-01` / `LOW-APP-01` `07` `08` `11` `14` `15` `17` `24`
