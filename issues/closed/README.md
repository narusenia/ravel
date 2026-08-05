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
| high | 18 | 1件1ファイル |
| medium | 20 | 領域別4ファイル（`medium-*.md`） |
| low | 5 | [low.md](low.md) 1ファイル |

## critical（4件 — 起票分すべて解決）

| ID | 内容 | 解決 |
|---|---|---|
| [CRIT-01](CRIT-01-eval-update-notifies-whole-workspace.md) | 評価結果ごとに全5パネルが再構築 | 第1段 / PR #191-193 |
| [CRIT-02](CRIT-02-save-failure-invisible-and-swallows-quit.md) | 保存失敗が不可視、Quit / Close を無言で破棄 | フェーズ A2 |
| [CRIT-03](CRIT-03-project-write-not-atomic.md) | 保存が非アトミック、`.bak` フォールバックなし | フェーズ A2 |
| [CRIT-04](CRIT-04-uncommitted-gesture-baked-by-foreign-commit.md) | 未コミットジェスチャーが他パネルのコミットで焼き付き | フェーズ A2 |

## high（18件）

| ID | 内容 | 解決 |
|---|---|---|
| [HIGH-03](HIGH-03-params-resolved-per-visit.md) | キャッシュヒット時もパラメータ全再解決 | `CACHE-2` |
| [HIGH-04](HIGH-04-per-frame-blocking-readback.md) | 表示フレームごとにブロッキングリードバック（毎回ステージング確保 + デバイス全体待ち + 二重コピー） | `GPUBK-6` |
| [HIGH-05](HIGH-05-shell-chain-cpu-per-pixel.md) | 殻の合成チェーンが CPU per-pixel | `GPUCOMP-2/3/5/6` |
| [HIGH-06](HIGH-06-pipeline-recompiled-per-param-edit.md) | パラメータ編集ごとにパイプライン再コンパイル | 第1段 |
| [HIGH-07](HIGH-07-document-changed-cascade-per-mouse-move.md) | マウス移動ごとに `document_changed` 全カスケード | 第1段 |
| [HIGH-08](HIGH-08-ui-thread-f32-to-bgra-conversion.md) | UI スレッドで全フレーム f32→BGRA 変換 | `GPUCOMP-9` |
| [HIGH-10](HIGH-10-audio-chunk-seek-wrong-time-base.md) | 音声チャンク seek の時間基準誤り | フェーズ A3 |
| [HIGH-11](HIGH-11-audio-chunk-no-trim.md) | 音声チャンクが要求位置までトリムされない | フェーズ A3 |
| [HIGH-12](HIGH-12-pause-does-not-stop-queued-audio.md) | Pause でキュー済み音声が止まらない | フェーズ A3（epoch） |
| [HIGH-13](HIGH-13-seek-does-not-flush-audio-queue.md) | Seek でチャンクキューを flush しない | フェーズ A3（epoch） |
| [HIGH-14](HIGH-14-clock-advances-over-underrun.md) | アンダーラン中も同期クロックが進む | フェーズ A3（epoch） |
| [HIGH-15](HIGH-15-settrack-resamples-on-prep-thread.md) | `SetTrack` が prep スレッドで全長リサンプル | フェーズ A3 / A4 |
| [HIGH-18](HIGH-18-open-failure-invisible.md) | プロジェクトを開けないが無言 | フェーズ A2 |
| [HIGH-20](HIGH-20-media-import-failure-invisible.md) | メディアインポート失敗が無言 | フェーズ A2 |
| [HIGH-21](HIGH-21-node-editor-repaints-every-playback-frame.md) | NodeEditor が再生中毎フレーム全再構築 | フェーズ A |
| [HIGH-22](HIGH-22-port-hit-test-ignores-z-order.md) | ポートヒットテストが z 順を無視 | フェーズ A |
| [HIGH-23](HIGH-23-resampled-audio-not-cached.md) | リサンプル結果が未キャッシュ | フェーズ A4 |
| [HIGH-24](HIGH-24-timeline-end-pause-not-forwarded-to-audio.md) | 終端の自動 Pause が音声へ転送されない | フェーズ A4 |

## medium（20件）

- [medium-app-shell.md](medium-app-shell.md) — `MED-APP-01` `12` `16` `18` `22` `23`
- [medium-core-evaluator.md](medium-core-evaluator.md) — `MED-CORE-02` `03` `06` `07` `09`
  （`MED-CORE-04` は 2026-08-03 に再判定して未解決へ戻した — デシリアライズ経路が
  無防備。[`../medium/core-evaluator.md`](../medium/core-evaluator.md)）
- [medium-gpu-nodes.md](medium-gpu-nodes.md) — `MED-GPU-01` `02` `03`
- [medium-media-audio.md](medium-media-audio.md) — `MED-MED-03` `04` `05` / `MED-AUD-01` `02` `03`

## low（6件）

[low.md](low.md) — `LOW-GPU-01` / `LOW-AUD-01` / `LOW-APP-01` `14` `15` `17`
