# 実装バックログ

全ライブ計画の実装単位を 1 枚に並べたもの。**着手できるものを探すための
ファイル**で、設計の正は各計画書にある。

- 単位の内容・完了条件は計画書を見る。ここには要約しか書かない。
- 計画書を更新したらこの表も更新する。片方だけ直さない。
- 完了した単位は行を消さず `✅` にして PR 番号を入れる。
- **順序の判断は `roadmap.md`**。この表は「何があるか」、ロードマップは
  「どの順でやるか、なぜその順か」。
- **`issues/` の項目はこの表に載せない**。issue は実装単位ではない（完了条件を
  持たない）ので、ロードマップがクラスタ単位で順序を決め、個票は `issues/` に
  置く。計画書が引き受けた issue だけ、該当単位の説明に ID が出る。

最終更新: 2026-08-25

## 凡例

| 記号 | 意味 |
|---|---|
| ✅ | マージ済み |
| 🟡 | 着手可能（依存が解決済み） |
| ⬜ | 依存待ち |
| ❓ | 前提条件の判断待ち（測定・設計決着など） |
| ❌ | 判断の結果やらないことにした（根拠は計画書に記録） |

## 今すぐ着手できるもの

依存が無いか、依存がすべて解決している単位。

| ID | 単位 | 計画 |
|---|---|---|
| TOOLX-2 | 矩形選択 | `viewer-tool-extensions-plan.md` |
| SCOPE-2 | 時間シフト経路（FX-5 の土台） | `evaluation-scope-plan.md` |
| SCOPE-3 | `geometry.iterate`（ピース単位反復） | `evaluation-scope-plan.md` |
| SIM-1 | `StatefulProcessor` と sim キャッシュの骨格 | `stateful-eval-plan.md` |
| BLUR-4 | `comp.motion_blur` と殻フィールド（BLUR-3 完了で着手可能） | `motion-blur-plan.md` |
| MOD-3 | 駆動ソース `field.time` / `field.constant` | `per-instance-modulation-plan.md` |
| MOD-4 | `attribute.delete`（属性列の削除） | `per-instance-modulation-plan.md` |
| ALIGN-1 | 整列・分布の計算（ヘッドレス） | `align-panel-plan.md` |
| VEC-4 | look-at・フロー場のゴールデン検証と文書（単位 1〜3・5〜8 が揃った） | `vector-field-plan.md` |
| STYLE-4 | 変調との結合検証と文書（`MOD-1` ✅ で依存が解けた） | `style-attributes-plan.md` |
| PSHADE-1 | パスの per-pixel 評価器（挙動不変。頂点色補間と `stroke_align` の土台） | `path-shading-plan.md` |
| OPS-1 | `geometry.blast`（要素削除） | `geometry-ops-plan.md` |
| OPS-2 | `geometry.sort`（並べ替え） | `geometry-ops-plan.md` |
| OPS-3 | `geometry.resample` | `geometry-ops-plan.md` |
| OPS-4 | `geometry.measure` | `geometry-ops-plan.md` |
| OPS-5 | `geometry.switch` / `geometry.null` | `geometry-ops-plan.md` |
| OPS-6 | `geometry.group_index`（index で要素指定） | `geometry-ops-plan.md` |
| OPS-7 | `geometry.repeat`（トランスフォームリピータ） | `geometry-ops-plan.md` |
| OPS-8 | デフォーマ（bend / twist / taper） | `geometry-ops-plan.md` |
| INFO-1 | `InvalidationHint::Shell`（挙動不変） | `scene-info-nodes-plan.md` |
| FX-3b | `comp.solid` / `comp.fill` / `comp.tint` / `comp.alpha` | `effects-library-plan.md` |
| SHELL-1 | `time_remap` の配線 | `layer-shell-wiring-plan.md` |
| SHELL-2 | `track_matte` の配線 | `layer-shell-wiring-plan.md` |
| SHELL-6 | レイヤー殻プロパティの式入力 UI（`EXPR-4` 完了で着手可能） | `layer-shell-wiring-plan.md` |
| UX-1 | 情報の所在表と往復候補の列挙（計器の材料） | `refactor-plan-0808.md` |
| NGR-4 | 型によるエッジ配色 | `node-graph-readability-plan.md` |
| NGR-6 | Reroute ノード | `node-graph-readability-plan.md` |
| NGR-7 | エッジへのドロップでノードを挟む | `node-graph-readability-plan.md` |
| WRG-1 | 式言語の複数文とローカル変数 | `wrangle-plan.md` |
| PATH-0a | ブーリアンの実装方針評価（依存判断） | `path-ops-plan.md` |
| GPUBK-13 | 文書更新（`GPUBK-14` の判定を要件・仕様へ反映） | `gpu-backend-plan.md` |
| GPUBK-15 | ディスパッチを 1 コンピュートパスに畳む | `gpu-backend-plan.md` |
| GPUBK-16 | ブロッキング読み戻しの 1 ms 切り上げを回収（`VRES-1` ✅ で条件は揃った） | `gpu-backend-plan.md` |
| OFX-0 | OFX の前提検証と Windows 経路の判断（ゲート） | `ofx-host-plan.md` |
| PLUG-1 | `ProcessorRegistry` と組み込みの移設 | `plugin-system-plan.md` |
| FX-1 | カラー調整とカラーグレーディング | `effects-library-plan.md` |
| FX-2 | ブラー / シャープ / ディストーション | `effects-library-plan.md` |
| FX-3 | 生成とスタイライズ | `effects-library-plan.md` |
| FX-4 | トランスフォーム拡張と合成（マスク / キーイング） | `effects-library-plan.md` |
| AUDIO-5 | 波形表示 | `audio-plan.md` |
| AUDIO-6 | 解析ノード（RMS / ピーク。**FFT クレート追加は禁止**） | `audio-plan.md` |
| 3D-4 | 三角形レンダラと `scene.render` | `3d-scene-plan.md` |
| FRAC-2 | `geometry.cell_fracture`（2D） | `geometry-fracture-plan.md` |
| FRAC-3 | `geometry.cell_fracture_3d`（`3D-1a` / `3D-1b` ✅ で依存が解けた） | `geometry-fracture-plan.md` |
| GPULOSS-3 | GPUI 採用 wgpu device の loss polling・再採用（`GPULOSS-2` ✅ で依存が解けた） | `gpu-device-loss-recovery-plan.md` |
| GPULOSS-4 | macOS は自前 device の loss で zero-copy を無効化し CPU fallback に留める | `gpu-device-loss-recovery-plan.md` |

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
第2段は `gpu-compositing-plan.md` に降りている（下表）。第3段は
`responsiveness-stage3-plan.md` に降りた（下表 `RESP3-*`）。

### 応答性 第3段（`roadmap.md` フェーズ C3）

`responsiveness-stage3-plan.md`。3 クラスタ、14 単位。クラスタ内は依存順、
クラスタ間は独立。

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| RESP3-1 | ✅ #395 | 隣接インデックスと `ptr_eq` キャッシュ（HIGH-01） | — |
| RESP3-2 | ✅ #395 | レイヤー単位の `ptr_eq` 短絡と親インデックス（HIGH-02） | — |
| RESP3-3 | ✅ #395 | パスのインターンと `Copy` な内部キー（MED-CORE-01） | RESP3-1 |
| RESP3-4 | ✅ #395 | `attribute_transfer` の空間分割と近傍打ち切り（MED-CORE-05） | — |
| RESP3-5 | ✅ #397 | sync 呼び出し回数の計装（MED-UI-06 のゲート） | — |
| RESP3-6 | ✅ #397 | `Params` ヒントでコンパイル済みチェーンを保持（MED-UI-01） | — |
| RESP3-7 | ✅ #397 | Properties の refresh 重複排除と非表示スキップ（MED-UI-02） | RESP3-5 |
| RESP3-8 | ✅ #397 | Timeline の垂直カリング（MED-UI-03） | — |
| RESP3-9 | ✅ #397 | Timeline の revision ゲート（MED-UI-04） | RESP3-5 |
| RESP3-10 | ✅ #397 | Outliner / MediaBin の revision ゲート（MED-UI-05） | RESP3-5 |
| RESP3-11 | ✅ #397 | グローバル駆動 sync の epoch 記録（MED-UI-06） | RESP3-9, RESP3-10 |
| RESP3-12 | ✅ #396 | rasterize ゴールデンの GPU / CPU 一致テスト化（MED-GPU-04 のゲート） | — |
| RESP3-13 | ✅ #396 | `finalize` の GPU ラスタライズと CPU 経路の bbox 限定（MED-GPU-04） | RESP3-12 |
| RESP3-14 | ✅ #396 | `ensure_gpu` のフレーム内メモ化（MED-GPU-05） | — |

`HIGH-17`（sws スケーラの毎フレーム再生成）は C3 のメディアデコード
クラスタだったが closed 済みで、単位を持たない。

**14 単位すべてマージ済みだが `RESP3-7` は完了条件を満たしていない** —
「パネル非表示のとき `refresh_values` を走らせない」が未達（タブの可視性が
パネルへ届かず、`ravel-dock` の配線が要る）。同じ理由でフェーズ C3 は
`進行中` のままで、`MED-UI-02` も未解決に残っている
（詳細は `roadmap.md` フェーズ C3 の `実施結果`）。`MED-UI-05` は
MediaBin 側を #400 で片付けて closed になった。

### パネル可視性（フェーズ C3 の残り）

`panel-visibility-plan.md`。裏のタブのパネルが払っている更新を止め、表に
戻ったときに取り返す。**`VIS-4` がフェーズ C3 を閉じる。**

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| VIS-1 | ✅ | `VisiblePanels` Global と `WindowHost` からの維持（挙動不変。#409） | — |
| VIS-2 | ✅ #461 | 可視性ゲートの共有ヘルパと Properties への適用（MED-UI-02） | VIS-1 |
| VIS-3 | ✅ #461 | Timeline / Outliner / MediaBin / NodeEditor への適用 | VIS-2 |
| VIS-4 | ✅ #461 | 仕様・実装状況・測定手順の文書、フェーズ C3 を閉じる | VIS-3 |

### GPU 合成パイプライン（`issues/README.md` 第2段）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| GPUCOMP-1 | ✅ | `perf_baseline` に N レイヤーのシェル合成シナリオを追加 | #197 |
| GPUCOMP-2 | ✅ #198 | `comp.opacity` の GPU 版 | GPUCOMP-1 |
| GPUCOMP-3 | ✅ #198 | `comp.transform` の GPU 版 + アルファ規約・タップ境界の是正 | GPUCOMP-2 |
| GPUCOMP-4 | ✅ #198 | `blur.wgsl` のアルファ規約統一（MED-GPU-02 の残り） | GPUCOMP-3 |
| GPUCOMP-5 | ✅ #199 | `comp.merge.*`（5モード）の GPU 版 | GPUCOMP-3 |
| GPUCOMP-6 | ✅ #199 | `comp.merge.adjustment` の GPU 版 | GPUCOMP-5 |
| GPUCOMP-7 | ✅ | リードバック回数と CPU/GPU 一致の回帰テスト | GPUCOMP-6 |
| GPUCOMP-8 | ✅ | リードバック実装の改善（HIGH-04） | GPUBK-6（#282）が回収 |
| GPUCOMP-9 | ✅ | f32→BGRA 変換を評価ワーカーへ（HIGH-08。HIGH-09 は一部） | #284 |
| GPUCOMP-10 | ❌ | 非同期リードバック（GPUBK-6 の測定で不要と判断） | — |
| GPUCOMP-11 | → | ゼロコピー表示（**引受先は `zero-copy-viewer-plan.md` の `ZC-*`**） | GPUCOMP-9 ✅ |

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

**`GPUCOMP-11` の現在地（2026-08-10 に整理）。** 元の単位は 2 つを抱えていた。

- **`VIEWER_MAX_DIM` の引き上げ** — **`VRES-1`（✅ #300）が回収済み。**
  定数そのものを撤去し、係数モデル（`ViewerResolution`）に置き換えた。
  判断の根拠は `GPUBK-9` の計測（`perf-baseline.md`、「常時フル解像度は
  目標に置かない」）で、そこから `done/viewer-preview-resolution-plan.md` が
  生まれている。**`VIEWER_MAX_DIM` という識別子はコードに存在しない**ので、
  文書で見かけたら過去の計測記録としてだけ読むこと
- **ゼロコピー表示** — **判断は `GPUBK-9`（✅ #296）で済んでいるが、実装の
  引受先が無い。** `gpu-backend-plan.md` の非対象節が「ゼロコピー表示の実装。
  `GPUBK-9` で判断し、必要なら別計画」と書いており、その別計画はまだ無い。
  `HIGH-09` の残りはこれ

`CM-3` の表示変換が rayon と境界表で 10.1× 速くなった（#363）ので、
**往復のうち CPU 変換が占めていた分は既に消えている。** ゼロコピー化の
判断はこの数字で測り直すこと。

### ゼロコピー Viewer 表示（HIGH-09 の残り）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| ZC-1 | ✅ #373 | 往復の内訳を `CM-7` 後の姿で測り直す（**判断ゲート**） | — |
| ZC-2 | ✅ #382 | gpui-ce に Metal デバイス / キューの取得口を足す | ZC-1 ✅ |
| ZC-3 | ✅ #384 | 出力テクスチャを GPUI のカスタム要素で描く（マージ時点では既定オフ） | ZC-2 ✅ |
| ZC-4 | ✅ #386 | 同期と寿命（フレーム跨ぎの取り違えを防ぐ。**既定オフを外した**） | ZC-3 ✅ |
| ZC-5 | ✅ #388 | Linux の経路（描画側のみ。**配線は ZC-8**） | ZC-3 ✅ |
| ZC-6 | ✅ #389 | 文書更新（`HIGH-09` の現在地。**クローズは ZC-7/8 の後**） | ZC-4 ✅, ZC-5 ✅ |
| ZC-7 | ✅ #391 | Windows の経路（実機あり。**確認は push して手動**） | ZC-5 ✅ |
| ZC-8 | ✅ #391 | 起動時に GPUI のデバイスを採用する（`REQ-GPU-001` の配線） | ZC-5 ✅ |

`CM-7`（#367）が表示変換を GPU へ移し、CPU の per-pixel 処理が経路から消えた。
**`ZC-1`（#373）が内訳を測り、ゲートは開いた。** リードバックと CPU 側の
包みだけで、しきい値（60 fps 予算 16.7 ms の 5% = 0.835 ms）を**全 18 セルが
超える** — 1080p Quarter で 1.88 倍、4K Full で 3.95〜4.64 倍
（`perf-baseline.md`）。GPUI のアップロードとアトラス churn は gpui-ce の
内部にありウィンドウ無しには測れないが、消える側にしか効かないので判断は
変わらない。凍結条件は成立せず、`ZC-2` 以降へ進む。`MED-GPU-07` は前提では
ない（解決済み）。障害は GPUI 側で、macOS の gpui が wgpu ではなく Metal
ネイティブであること。

**`ZC-2`（#382）が macOS 側の障害を取り除いた。** フォークの
`Window::native_gpu_handles()` がレンダラの `MTLDevice` / `MTLCommandQueue` を
返し、`interop::context_from_native` が**同じネイティブデバイスを持つ wgpu
アダプタを列挙から照合して** `GpuContext` を立てる（wgpu 29 の公開 API には
既存 `MTLDevice` からアダプタを作る道が無いため）。ただし GPUI は低消費電力を、
Ravel は HighPerformance を優先するので、**複数 GPU の Mac では照合が失敗して
`None` になる** — `ZC-3` はその場合に CPU 経路へ落とす必要がある。

**`ZC-3`（#384）が経路を通した。ただし既定はオフ。** フォークの surface 経路は
macOS では NV12 動画専用だったので、RGBA テクスチャの腕とシェーダを足した
（`ZC-2` で終わらず、この単位にもフォークパッチが要った — 計画書はそれを
書いていない。`ZC-6` で直す）。止めているのは能力ではなく**寿命**で、
リースの解放が GPUI のコマンドバッファ完了を待たない。独立レビューと
CodeRabbit が別々に同じ欠陥を挙げたため、`workspace.rs` の既定を `false` に
固定してマージした。デバイス照合は `capability` としてログに残る。
したがって**「実行中のアプリでリードバックが 0」は `ZC-4` へ繰り越し**
（ワーカー側は `crates/ravel-nodes/tests/display_surface.rs` が証明済み）。
`ZC-4` はその既定を外すところまでを担う。

**`ZC-4`（#386）が寿命を閉じ、既定を外した。** フォークの
`SurfaceSource::Texture` が完了コールバック（`Arc<dyn Fn()>`）を運び、
`gpui_macos` の 3 つの draw 経路すべてで Metal の `add_completed_handler` に
登録される。Ravel 側は `GpuFrameBuffer::completion_signal()` が**フレームの
クローンを捕獲した**コールバックを返すので、GPUI がコマンドバッファを
retire してブロックが解放されるまでテクスチャはプールへ戻らない。
GPUI はコールバックの中身を知らないため、上流へ出せる汎用 API の形を保っている。
あわせて `wait_for_pending()` が `ZC-3` の残した全体待ちを submission 単位へ狭めた。
**実機で 299 フレーム再生してティアリングなし**、これで「実行中のアプリで
リードバックが 0」も満たした。ただし**デバイス喪失・ウィンドウ再作成の
自動テストは無い**（headless で再現できない）。実装はフォールバックで
受けているが未検証。**クローズは `ZC-7` / `ZC-8` の後**で、`ZC-6` は現在地を書くところまで。

### キャッシュ（REQ-CORE-006）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| CACHE-1 | ✅ | `FrameBuffer` の精度多相化（規約のみ。`as_f32` アクセサ + lint） | — |
| CACHE-2 | ✅ | `CacheIdentity` の抽出と時間基準化（旧 BLUR-2、HIGH-03） | — |
| CACHE-3 | ✅ #230 | `CacheBudget` と退避（MED-CORE-06 / 07） | CACHE-1, CACHE-2 |
| CACHE-4 | ✅ #227 | スコープ無効化の粒度修正（MED-CORE-02） | CACHE-2 |
| CACHE-5 | ✅ #366 | フレームキャッシュ層（comp 単位の無効化） | CACHE-3 ✅, GPUCOMP-7 ✅ |
| CACHE-6 | ✅ #366 | Timeline のキャッシュ帯と `cache_stats` | CACHE-5 ✅ |
| CACHE-7 | ✅ #370 | 無効化を時間範囲に絞る | CACHE-5 ✅ |
| CACHE-8 | ✅ #368 | 共有デコードフレームキャッシュ（HIGH-16 / MED-MED-02） | CACHE-3 ✅ |
| CACHE-9 | ✅ #370 | 先読み（投機充填。中断可能） | CACHE-5 ✅ |
| CACHE-10 | ✅ #370 | 文書更新 | CACHE-7 ✅ |
| CACHE-Y | ❓ | per-pixel ループの format 汎用化（実測後。他は依存しない） | CACHE-1 |
| CACHE-11 | ❓ | ディスク層（測定ゲート） | CACHE-5 ✅ |

CACHE-1 は `3D-1a` / `3D-1b` と同じ理由で早いほど安い（`FX-1`〜`FX-4` が
per-pixel ループを増やす前に規約を確定させる）。`FrameBuffer` は
`Serialize` を持たないので**永続化フォーマットの移行は無い**。

CACHE-2 は済み。有効判定は `TimeKey`（1/4096 フレーム）と `Precision` を軸に
持つ `CacheIdentity` になり、旧 BLUR-2・MED-CORE-03・HIGH-03 を回収した。
BLUR-3〜5 のゲートは開いた。

CACHE-3 / 4 も済み（#230 / #227）。評価キャッシュは `CacheBudget` の下で
バイト会計され LRU で退避される（MED-CORE-06）。`register()` の全走査は
`NodeId → paths` の逆引き索引で消え、スコープ状態も prune される
（MED-CORE-07）。バインディング差し替え時の無効化は到達集合に絞られた
（MED-CORE-02）。**`settings.toml` の `[cache]` は走行中の予算へ届く**
（`SET-8` / #374。VRAM・RAM の上限と sim 予約率、置き場。ディスク層だけは
課金経路が無いので `CACHE-11` 待ち）。ただし `ravel-cli` は今も既定から
予算を作るので、ヘッドレスレンダーは `[cache]` を無視したまま。

### 設定画面と設定の適用（REQ-PROJ-004）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| SET-1 | ✅ | 設定の適用経路と言語（UI なし。MED-APP-10 の中核） | #276 |
| SET-2 | ✅ | 設定ダイアログの骨組み（`gpui_component::setting::Settings`） | #275 |
| SET-3 | ✅ | 外観（テーマモード / テーマ選択） | #278 |
| SET-4 | ✅ | 言語の切り替え UI | #278 |
| SET-5 | ✅ | キーバインドのユーザー上書きと一覧（LOW-APP-15） | #277 |
| SET-6 | ✅ | プロジェクト設定画面（既定フレームレート） | #279 |
| SET-7 | ✅ | 文書更新 | #279 |
| SET-8 | ✅ #374 | キャッシュ設定（ディスク層は `CACHE-11` 待ちで据え置き） | CACHE-3 ✅ |
| SET-9 | ❓ | 自動保存（間隔 / 無効化） | REQ-PROJ-002 のタイマー実装 |
| SET-10 | ❓ | プロキシ設定 | プロキシ生成の実装 |
| SET-11 | ❓ | カラー設定（OCIO） | カラー管理の実装 |
| SET-12 | ❓ | キーバインドの割り当て編集 | SET-5（別計画に切り出す判断もあり） |
| SET-13 | ❓ | 設定の import / export | 項目が揃ってから |
| SET-14 | ❓ | UI スケーリング | 調査（パネルが `Theme.font_size` を尊重しているか） |
| SET-15 | ❓ | 色覚多様性テーマ / アニメーション削減 | テーマ資産の追加とアニメーション箇所の棚卸し |
| SET-16 | ✅ #361 | 停止位置と起動時コンポの設定を設定画面へ出す | UX-11 ✅ #352 |

**「出す項目 = 実際に効く項目」が規約**なので、SET-9 以降は前提機能の
マージ後に着手する（`settings-screen-plan.md`）。同じ規約により、`SET-8` は
キャッシュ設定のうち**効く 4 項目だけ**を出し、ディスク層は据え置いた。

**`SET-1`〜`SET-7` は済み**（#276 / #275 / #278 / #277 / #279）。**環境設定から
言語と外観を、プロジェクト設定から既定フレームレートを切り替えられ、その場で
反映される** — `ja.toml`（235 キー）が製品内から到達可能になり、`MED-APP-10` の
中核が消えた。起動時に `default → global → project` が解決されて `AppSettings`
Global に載り、層ごとに独立した書き込み API（失敗は通知）がある。テーマは名前で
レジストリのエントリを渡すのでホットリロードが効き、無効な名前は同梱テーマへ
フォールバックする。キーバインドは `<config>/ravel/keybindings.toml` が既定へ
重なり（壊れた行はその行だけ捨てる）、環境設定に読み取り専用の一覧が出る
（`LOW-APP-15` 解決）。「既定に戻す」はどの項目でもその層の値を消す（既定値を
書き戻さない）。既定フレームレートは**アクティブなコンポジションがあるとその書式が
勝つ**ので、開いている状態では観測できない（意図。`SET-6`）。

**`SET-8` も済み**（#374）。キャッシュの VRAM / RAM 上限、sim 予約率、置き場が
環境設定に出て、`SharedCacheBudget::reconfigure` で走行中の予算へ届く。
設定ファイルに直接書かれた範囲外の値と相対パスも同じ規則で弾かれる。

残るのは**ゲート付きの `SET-9`〜`SET-15` の 7 件**だけで、前提機能が入るまで
着手しない。`user` 層も未実装のまま（マシン固有 / CLI 上書き用の枠）。
**`MED-APP-10` 自体はまだ閉じない** — `auto_save` / proxy / color が依然未配線で、
`SET-9`〜`SET-11` が残っている
（`SET-8`。`CacheBudget` 自体は `CACHE-3` で入っている）。

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
| OPS-11 | ✅ | `shape.line` / `shape.grid`（表の「生成 ✅」は誤りだった）（#406） | — |
| OPS-12 | ✅ | `geometry.connect`（要素をベジエ/直線で結ぶ。Add SOP 相当）（#406） | — |
| OPS-13 | ✅ | `attribute.curveu`（パスパラメータ `u` の予約と書き込み）（#406） | — |
| OPS-10 | ⬜ | レジストリ / ロケール / 文書 | OPS-1〜9, OPS-11〜13 |

### 塗り・線のスタイル属性化

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| STYLE-1 | ✅ | スタイル属性の読み出し（CPU / GPU）（#403） | — |
| STYLE-2 | ✅ | `style.fill` / `style.stroke` ノード（#417） | STYLE-1 |
| STYLE-3 | ✅ | ダッシュ・キャップ・ジョイン（#417。`stroke_align` は `PSHADE-3` へ移動） | STYLE-1 |
| STYLE-4 | 🟡 | 変調との結合検証と文書 | STYLE-2 ✅, MOD-1 |
| STYLE-5 | ✅ | `field.apply` の属性自動作成 + Color 既定マスクを `rgb` へ（#403） | — |
| STYLE-6 | ✅ | `field.ramp`（位置 → 色のランプ）（#408） | STYLE-5, VEC-1 |

### パスのシェーディング（頂点色補間と `stroke_align`）

`path-shading-plan.md`。CPU 経路が per-pixel の幾何情報を持たない
（zeno が被覆マスクしか返さない）ことで止まっている 3 つを 1 本にまとめる。

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PSHADE-1 | 🟡 | `path_sample()`（CPU の per-pixel 評価器）と WGSL 側の情報追加（挙動不変） | — |
| PSHADE-2 | ⬜ | 線の頂点色補間（CPU / GPU）。`MED-GPU-08` の本体 | PSHADE-1 |
| PSHADE-3 | ⬜ | `stroke_align`（`style-attributes-plan.md` 単位 1 からの繰り延べ） | PSHADE-1 |
| PSHADE-5 | ⬜ | ゴールデンの拡張と文書、`MED-GPU-08` を閉じる | PSHADE-2, PSHADE-3 |
| PSHADE-6 | ⬜ | **要素ごとのグラデーション塗り**（位置由来。軸は Primitive の Vec2 属性、評価はジオメトリ空間） | FX-3, STYLE-2 |

`PSHADE-4`（塗りの頂点色）は案 A の採用により欠番。

塗りの方式は案 A（線だけ。塗りはプリミティブ色のまま）に確定した。塗りに
Point ドメインの `Cd` を書いた場合は無視する。現状もプリミティブ値 1 色で描いて
おり、平均や先頭を採ると既存の絵が黙って変わるためで、この扱いは `PSHADE-5` の
文書更新で利用者向けに明記する。

STYLE-5 の「Color 既定マスクを `rgb`」は**既定値の変更**。現状スカラー
フィールドは Color の全 4 成分に broadcast され、明度と同時にアルファも動く
（`crates/ravel-core/src/geometry/field.rs:686-688`）。

### ベクタ場

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| VEC-1 | ✅ | 二項合成の多相化（**Color / Vec4 を含む**）（#405） | MOD-2 |
| VEC-2 | ✅ | 変換ノード（length / component / compose / angle）（#415） | VEC-1 |
| VEC-3 | ✅ | ベクタ場（direction_to / curl_noise / gradient / radial）（#420） | VEC-2 |
| VEC-7a | ✅ | `vector.construct.vec2` / `vec3` / `vec4`（値ドメイン。VEC-5 の移行が挿入する） | — |
| VEC-5 | ✅ | Vec パラメータの正規化（`_x`/`_y` → `Channel2` / `Channel3`、`Channel3`→VEC3 ポート、`attribute.set` の型駆動 `value` と再型付け、format v5 マイグレーション） | VEC-7a |
| VEC-6 | ✅ | `constant.vec2` / `vec3` / `vec4`（#402） | VEC-5 |
| VEC-7b | ✅ | `vector.split` / `swizzle`（値ドメイン）（#412） | VEC-6, NETIF-1 |
| VEC-8 | ✅ | `vector.length` / `normalize` / `dot` / `cross`（値ドメイン）（#412） | VEC-6 |
| VEC-4 | 🟡 | look-at・フロー場のゴールデン検証と文書 | VEC-3, VEC-5〜8 |

**VEC-7a を VEC-5 より先に置いているのは循環を切るため**。VEC-5 の移行は
「`center_x` と `center_y` の両方に別ノードが繋がっている旧ファイル」で
`vector.construct` を挿入する必要がある。`construct` は Scalar 入力と Vec
出力だけで成立し `constant.vec*` を要らないので、単位 7 から切り出せる。
アリティは `type` パラメータではなく `type_key` で分けた（ポート型が
ノードインスタンスに保存されるため。計画書の単位 7 に根拠を記載）。

**VEC-5 は 2 つの計画のゲート**で、両方の前提が満たされた。組み込みノードの
Vec は `Channel2` / `Channel3` の 1 パラメータになったので、
`done/viewer-overlay-manipulator-plan.md` の `ParamRole`（1 パラメータ = 1 つの意味）
は OVL-5 で宣言できる。Properties の Vector 行（横並び）も実際に到達する
ようになった。`attribute.set` の `value` も `type` に従うアリティで畳み、
`type` 変更時の再型付け（値・ポート型・不整合エッジの破棄を 1 コマンドで）を
同じ単位に入れた。畳まないのは `Int` の成分対（`scatter.grid` の
`count_x` / `count_y`）だけ。

### ネットワークインターフェース編集（REQ-LAYER-002 / 003）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| NETIF-1 | ✅ | 出力ポートの再インデックス API（入力側は既存） | — |
| NETIF-2 | ✅ | In / Out のカスタムポート編集 API + 型の文脈依存 | NETIF-1 |
| NETIF-3 | ✅ | Properties の Ports セクション | NETIF-2 |
| NETIF-4 | ✅ | ポート右クリック（Rename / Delete） | NETIF-2 |
| NETIF-5 | ✅ | Subnet の生成と `sync_subnet_pins` | NETIF-1 |
| NETIF-6 | ✅ | Collapse / Extract（#304） | NETIF-5 |
| NETIF-7 | ✅ | レジストリ / ロケール / 文書（#307） | NETIF-1〜6 |

評価側は完成しており（`net.in` のカスタムポート、`net.out` の
`PortRecord`、`subnet` の再帰評価）、**コア側の編集 API も揃った**。

- `NETIF-1`: 出力ポートの再インデックス（`Graph::remove_output_port` /
  `insert_output_port` / `rename_port` / `reorder_ports`）。`Edge::source_port`
  と、グラフから見える `ChannelSource::NodeOutput`（`Node::parameter_sources`
  で辿れるもの）をまとめて remap する。`Layer` の殻チャンネルが持つ
  バインディングは `Graph` から見えないので追従しない — 計画書の単位 1 を見よ
- `NETIF-2`: `network::add_custom_port` / `remove_custom_port` /
  `rename_custom_port` と、`NetworkContext` による許可型の文脈依存判定、
  固定ポートの保護、未接続ポートの型付きゼロ
- `NETIF-3`: Properties の Ports セクション（追加・改名・型変更・並び替え・
  削除、1 操作 1 undo）と、それが要求した `set_custom_port_type` /
  `move_custom_port`。**カスタムポートはこれでテストフィクスチャと
  デモデータの外に出た**

- `NETIF-4`: ノードエディタのポート右クリック（Rename / Delete）。項目は
  隠さず無効化し、判定は `is_fixed_port` と In / Out 判定だけ。ポート一覧が
  変わったら進行中のワイヤードラッグと改名エディタを畳む（`PortHit` が
  持つ port index が古くなると、`add_edge` が検証しないので黙って死ぬエッジが
  できる）
- `NETIF-5`: `create_node` が Subnet に内部グラフを与え、`sync_subnet_pins` が
  ピンを内部 In / Out から導出する。ロード時のドリフト修復付き。
  **Add Node から作った Subnet がそのまま評価できるようになった**

- `NETIF-6`: Collapse to Subnet / Extract Subnet。境界を横切るエッジからピンを
  導出し、外側の端点ごとに 1 ピンへ束ねる（外側から見た配線本数が変わらない）。
  **既にあるグラフを畳む / 展開する手段が開いた**

残るのは `NETIF-7`（掃き寄せ — レジストリ / ロケール / 文書）だけ。
`NETIF-6` が残した既知の制限（境界を越えるパラメータ束縛の未追従など）は
計画書の単位 6 に記録してある。

### シーン情報ノード（REQ-LAYER-002 / 005）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| INFO-1 | 🟡 | `InvalidationHint::Shell`（挙動不変で経路を通す） | — |
| INFO-2 | ⬜ | `layer.info` | INFO-1, NETIF-2 |
| INFO-3 | ⬜ | `comp.info` | INFO-1 |
| INFO-4 | ⬜ | 情報ノードのポート選択 UI | INFO-2, NETIF-3 |
| INFO-5 | ⬜ | 殻バインドを含む循環検出 | INFO-2 |
| INFO-6 | ⬜ | レジストリ / ロケール / 文書 | INFO-2〜5 |

殻の transform / 時間配置編集は現在 `InvalidationHint::None`
（`panels/properties.rs:839-842`）。情報ノードは殻フィールドをグラフの入力に
するので、INFO-1 が無いと参照側が古い値のままになる。

### ポインタフィードバック（フェーズ A5）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PTR-1 | ✅ | 判定不要の静的カーソル（ツール中 Viewer / ルーラー / NodeEditor 空白、PR #213） | — |
| PTR-2 | ✅ | ヒント機構とドラッグ中の保持（Timeline で導入、PR #213） | — |
| PTR-3 | ✅ | Timeline の割り当て（トリムエッジ / バー / ロック / キーフレーム / グラフ、PR #213） | PTR-2 |
| PTR-4 | ✅ | NodeEditor の割り当て（ポート / ノード / エッジ / パン、PR #213） | PTR-2 |
| PTR-5 | ✅ | Viewer の割り当て（レイヤー移動 / パスハンドル / パン / ペン閉合、PR #213） | PTR-2 |
| PTR-6 | ✅ | Outliner の並べ替えと文書（`ui-spec.md` / `gpui-ui-guide.md`、PR #213） | PTR-3〜5 |

hover 判定は既存ヒットテストの再利用に限り、新しいレイアウト走査を作らない
（`MED-APP-13` を悪化させない）。Hand / Zoom（`MED-APP-15`）と Viewer bbox の
8 ハンドルは**操作が未実装なのでカーソルを付けない** — フェーズ E で機能と
同じ単位に入れる。

### Viewer の表示オプションと検査

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| INSP-1 | ✅ | `background_color` の配線とチェッカーボード（MED-CORE-09、PR #213） | — |
| INSP-2 | ✅ | チャンネル単独表示（R / G / B / A / マット、PR #479） | INSP-1 |
| INSP-3 | ✅ | ピクセル値の読み取り（PR #480） | OVL-1 |
| INSP-4 | ✅ | 再生とキャッシュの状態表示（PR #482） | （キャッシュ表示のみ CACHE-6 ✅） |
| INSP-5 | ✅ | スコープ 4 種の引き取り判断 → 引き取らず `viewer-scopes-plan.md` へ | — |

INSP-1 は**設定できるのに効かない**フィールドの解消なので、他の検査機能より
先に入れる（`roadmap.md` フェーズ A5）。表示オプションは `.ravprj` にも
`ui_state.json` にも保存しない（セッション内のパネル状態）。

**この計画は閉じた**（`done/`）。`INSP-5` は 4 種を引き取らず
`viewer-scopes-plan.md` に切る判断で終わっている。

### スコープ 4 種（波形 / ベクトル / ヒストグラム / パレード）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| MON-1 | 🟡 | リニアフレームの要求を数える（読み取りとスコープで 1 本のコピーを共有） | — |
> **スコープ 4 種は 2026-08-25 のユーザー判断で後回し**（作るコストの割に
> 入れるものがない）。`MON-1` は依存が無く技術的には着手可能だが、
> **上の「今すぐ着手できるもの」には載せていない** — 依存が解けていることと
> 「今やるべきこと」は別。再開はユーザーの指示で。
| MON-2 | ⬜ | 要約の計算（ヘッドレス、サンプル数上限つき） | MON-1 |
| MON-3 | ⬜ | ヒストグラムパネル（要約の共有経路をここで通す） | MON-2 |
| MON-4 | ⬜ | 波形モニタ | MON-2 |
| MON-5 | ⬜ | パレード（MON-4 の描画を 3 チャンネル分） | MON-4 |
| MON-6 | ⬜ | ベクトルスコープ | MON-2 |
| MON-7 | ⬜ | 受入条件と文書（REQ-UI-004 の 4 項目） | MON-3〜MON-6 |

トグルとレイアウトは既にある（`PanelKind` の 4 種、`Alt+6`、`color.toml`）。
足りないのは中身だけ。OCIO（REQ-UI-004 の「反映」）は本計画の非対象で、
色管理の計画ができてから単位を足す。

### Viewer のプレビュー解像度

計画: [`done/viewer-preview-resolution-plan.md`](done/viewer-preview-resolution-plan.md)
（**完了** — 2026-08-24）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| VRES-1 | ✅ | 係数モデルと評価経路（`VIEWER_MAX_DIM` の撤去） | #300 |
| VRES-2 | ✅ | 係数の UI とコマンド | #473 |
| VRES-3 | ✅ | UI 状態の永続化（`ui_state.json`） | #474 |
| VRES-4 | ✅ | 適応解像度（入力中は落とす） | #475 |
| VRES-5 | ✅ | 文書更新と `REQ-UI-004` の受入条件 | #476 |

隠し定数 `VIEWER_MAX_DIM = 1024` は `ViewerResolution`（`Full` / `Half` /
`Quarter`、既定 `Half`）に置き換わり、ツールバーのセレクトと `Alt+R`
（循環コマンド）で選べて `ui_state.json` に残る。操作中は 1 段落ちて
最後の入力から 120 ms で戻る。**係数ごとの実測は `perf-baseline.md` の
「プレビュー解像度の係数ごと」節**（1080p で `Full` 12.65 ms /
`Half` 3.74 ms / `Quarter` 1.85 ms）。実機確認済み。

### Viewer のスナップとガイド

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| SNAP-1 | ✅ | 既存要素へのスナップ（他レイヤー / コンプ枠 / セーフエリア）（#444。抑制キーは Alt ではなく Cmd / Ctrl、拘束修飾が効く経路では吸着しない） | OVL-1 |
| SNAP-2 | ✅ | 定規とユーザーガイド（`Composition` へ追加フィールド、format v8 据え置き）（#446。表示 / ロックはセッション状態、操作は `Select` 限定） | SNAP-1 |
| SNAP-3 | ✅ | ロケールと文書（#446） | SNAP-1, SNAP-2 |

SNAP-2 は永続化を触ったが**追加フィールド + `serde(default)`** なので
format version もマイグレーションも増えていない（`Layer.audio` の前例）。

### Viewer ツールの拡張

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| TOOLX-1 | ✅ | Hand / Zoom ツールの実装（MED-APP-15 closed、PR #484） | — |
| TOOLX-2 | 🟡 | 矩形選択 | OVL-1 |
| TOOLX-3 | ⬜ | ヒット対象のフォールバックと点の挿入 / 削除 / ハンドル分離 | — |
| TOOLX-4 | ⬜ | polygon / star のドラッグ描画 | — |
| TOOLX-5 | ⬜ | ロケールと文書 | TOOLX-1〜4 |

TOOLX-1 は `MED-APP-15` を引き受ける単位。`done/pointer-feedback-plan.md` が
見送った Hand / Zoom のカーソルもここで入る。REQ-UI-011 が v1.5 / v2 に
送った項目の引受先が無かったので、この計画がまとめて持つ。

### ノードの発見性と説明

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| DISC-1 | ✅ #251 | ノードのロケールキー化（label / description / params、40 テンプレート） | — |
| DISC-2 | ✅ #252 | ホバー Popover（説明・ポート・パラメータ現在値） | DISC-1 |
| DISC-3 | ✅ #253 | ノード検索パレット（Tab / ダブルクリック、型フィルタ） | DISC-1 |
| DISC-4 | ✅ #254 | 文書 | DISC-1〜3, DISC-5 |
| DISC-5 | ✅ #250 | ノードアイコン（`for_node_type`、カテゴリ既定フォールバック、ヘッダ描画） | — |

DISC-1 は `LOW-APP-11`（ハードコード英語）のうちノード名・説明・パラメータ名の
層だけを回収する。コア層はロケールを知らないまま（キー解決は UI 側）。
DISC-5 も同じ向きで、アイコンの対応表は UI 側に置き `NodeTemplate` は
アイコンを持たない。

### Viewer オーバーレイ機構とマニピュレータ

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| OVL-1 | ✅ #255 | オーバーレイ機構の抽出（挙動不変のリファクタ） | — |
| OVL-2 | ✅ | オーバーレイ用の評価要求（multi-target に相乗り）（#429。機構はドーマント — OVL-3 の前提を計画書に記載） | OVL-1, SHEET-1 |
| OVL-3 | ✅ | Geometry オーバーレイ + `shape_node_bounds` の廃止（#437。`MED-APP-21` を close、`EvalRequest` にスコープ付きターゲットを追加） | OVL-2 |
| OVL-4 | ✅ | Field オーバーレイ（#437） | OVL-2 |
| OVL-5 | ✅ | `ParamRole` とマニピュレータ（#435。`Position` / `Size` のみ — `Direction` / `Angle` は実装する単位が宣言と同時に足す） | OVL-1, VEC-5 |
| OVL-7 | ✅ | レイヤー殻のマニピュレータ（scale / rotation / anchor）+ HUD + 親子リンク線（#432） | OVL-1 |
| OVL-8 | ✅ | ジオメトリ属性の空間可視化（矢印 / index / group）（#439） | OVL-3 |
| OVL-9 | ✅ | モーションパス（軌跡表示 + キー位置のドラッグ。空間ベジェは持たない）（#439） | OVL-1, OVL-7 |
| OVL-6 | ✅ | ロケール / 文書（#441） | OVL-1〜5, OVL-7〜9 |

OVL-2 は `EvalRequest` を触る 3 つ目の計画。独自経路は作らず
`done/attribute-spreadsheet-plan.md` 単位 1 の multi-target 化に乗る。

OVL-7 は選択 bbox の 8 ハンドルを**初めて機能させる**単位（現状は描画だけで
スケール・回転のジェスチャーが存在しない）。`VEC-5` には依存しない — 殻は
最初から `[AnimationChannel; 2]`。`done/pointer-feedback-plan.md` が保留した
`Resize*` / 回転カーソルもこの単位で入る。

### Properties の複合パラメータエディタ

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PARAM-1 | ✅ | `ParameterValue::Curve` と文字列からのマイグレーション（format v6） | — |
| PARAM-2 | ✅ | カーブエディタのインライン展開（アコーディオン） | PARAM-1 |
| PARAM-3 | ✅ | `ParameterValue::Ramp` と `field.ramp`（`STYLE-6` と同じ実装。#408 が `RampParam` と両方の完了条件を入れた） | PARAM-1, STYLE-6 |
| PARAM-4 | ✅ | グラデーションエディタのインライン展開（#414） | PARAM-3 |
| PARAM-5 | ✅ | カーブエディタの表示範囲を Timeline と共有（`widgets/curve_view.rs`。Timeline 側のホイール縦ズームは `MED-APP-17` に残る） | PARAM-2 |
| PARAM-7 | ✅ | `math.curve`（値ドメインの curve remap）（#404） | PARAM-2 |
| PARAM-8 | ✅ #459 | `color.ramp`（値ドメインのカラーランプ。Blender ColorRamp 相当） | PARAM-4 |
| PARAM-6 | ⬜ | ロケール / 文書 | PARAM-1〜5, PARAM-7〜8 |

`field.curve_remap` の制御点は `PARAM-1` で `ParameterValue::Curve` になり
（旧 `"0:0,1:1"` 文字列は `.ravprj` v5 → v6 で変換）、`PARAM-2` で
Properties のインラインカーブエディタ
（`crates/ravel-app/src/widgets/param_curve_editor.rs`）から編集できる。
Timeline の `widgets/curve_editor.rs` とは座標変換と評価関数を共有する
（実装が分かれた理由は計画書 単位 2）。

**2 型に 6 つの消費者がいる**。カーブとランプがそれぞれ 3 ドメインに現れる。

|  | 値 | Field | Raster |
|---|---|---|---|
| Curve | `math.curve`（PARAM-7） | `field.curve_remap`（実装済み） | トーンカーブ（FX-1） |
| Ramp | `color.ramp`（PARAM-8） | `field.ramp`（STYLE-6） | グラデーション（FX-3） |

**FX-1 / FX-3 / STYLE-6 は PARAM-1 の型を使う。** 別表現を作ると
カーブ / ランプの表現とエディタがドメインごとに分裂する。

### パス操作

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PATH-0a | 🟡 | **ブーリアンの実装方針評価**（依存追加の可否含む） | — |
| PATH-0b | ✅ | **三角形分割器の採用判断**（#301 — `earcut` 採用、依存追加は承認済み） | — |
| PATH-1 | ❓ | `path.boolean` | PATH-0a = クレート採用 |
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
| SHELL-5 | ✅ | `parent` の設定 UI（Properties の Parent ドロップダウン、循環候補を除外）（#303） | — |
| SHELL-4 | ⬜ | 文書更新 | SHELL-3, SHELL-5 |
| SHELL-6 | 🟡 | レイヤー殻プロパティの式入力 UI | EXPR-4 ✅ |

SHELL-6 は SHELL-5 と**同じ向きの取り残し**。殻のチャネル（位置・スケール・
回転・不透明度・アンカー）は**既に式を評価する**のに、式を付け外しする経路が
ノードパラメータ側にしか無い（`EXPR-4`、#320）。作業は `EXPR-4` が作った
draft / attach / detach の機構を殻のコミット経路へ載せ替えることで、
`EXPR-4` 本体とは独立に閉じられる。**AE 的には位置と不透明度こそ式の主戦場**
なので、REQ-MOGRAPH-001 を実質満たしたと言えるのはこの単位が入ってから。

SHELL-5 は他の 3 つと**向きが逆の取り残し** — `parent` は評価では効くのに
設定 UI がどこにも無い（基準 4「評価はできるが編集できない」）。Viewer の
親子リンク線は `OVL-7` が持つので、この単位は設定手段だけ。

### モーションブラー（REQ-RENDER-004）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| BLUR-1 | ✅ | アニメーションチャネルの連続時間化 | #187 |
| BLUR-2 | — | **`cache-plan.md` の CACHE-2 に統合** | — |
| BLUR-3 | ✅ | 品質段階 `EvalContext.quality` | #311 |
| BLUR-4 | 🟡 | `comp.motion_blur` と殻フィールド | BLUR-3 ✅ |
| BLUR-5 | ⬜ | 文書更新 | BLUR-4 |

BLUR-2（キャッシュ有効性を `time` 基準へ）は `cache-plan.md` の CACHE-2 に
統合し、そこで実装した。同じ有効判定を 2 計画で別々に書き換えると衝突するため。
これが無いと BLUR-3〜5 は「実装したのにブレない」形で静かに壊れていた
（キャッシュが整数 frame を見ているため 2 サンプル目以降がヒットする）。
BLUR-3 の `quality` は CACHE-2 の `CacheIdentity` に軸として足す。

### 書き出しと CLI（REQ-RENDER-001 / REQ-RENDER-005）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| EXPORT-0 | ✅ | 永続化を GUI 非依存クレートへ抽出（`crates/ravel-project`） | #299 |
| EXPORT-1 | ✅ | エンコーダ抽象と実行時列挙 | #313 |
| EXPORT-2 | ✅ | レンダーワーカーとキュー | #314 |
| EXPORT-3 | ✅ | **CLI（`ravel-cli render`、別バイナリ）** | #325 |
| EXPORT-4 | ✅ | 音声のミックスダウンと多重化 | #328 |
| EXPORT-5 | ✅ | 書き出し UI | #332 |
| EXPORT-6 | ✅ | 文書更新 | #335 |
| EXPORT-7 | ✅ | CLI の対話モード | #330 |

### CI キャッシュ

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| CICACHE-1 | ✅ | sccache を導入し `target/` のアーカイブを外す | #334 |
| CICACHE-2 | ✅ #339 | 効果の計測と設定の詰め（cold/warm 実測、R2 見積もり、`line-tables-only`） | CICACHE-1 ✅ |

### 離散パラメータのキーフレーム（`done/discrete-keyframes-plan.md`）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| DISK-1 | ✅ #457 | `IntChannel` と解決層（フォーマット上げ。`.ravprj` v10） | — |
| DISK-2 | ✅ #462 | `StepCurve<String>` と `StringSteps` | DISK-1 ✅ |
| DISK-3 | ✅ #462 | Properties のキーフレームトグルと再型付け | DISK-1, DISK-2 |
| DISK-4 | ✅ #465 | Timeline の行とキーフレーム編集 | DISK-3 ✅ |
| DISK-5 | ✅ #465 | カーブエディタの階段描画（Int のみ） | DISK-4 ✅ |
| DISK-6 | ✅ #465 | ロケール / 文書 | DISK-1〜5 ✅ |

### レンダーの警告経路（`done/render-warning-channel-plan.md`）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| WARN-1 | ✅ #467 | 識別子パラメータの解決を 1 経路に畳む（`HIGH-35`。予約と評価の一致） | — |
| WARN-2 | ✅ #467 | `ravel-cli render` の映像側 `Warning` と静的走査（`HIGH-34`） | WARN-1 ✅ |
| WARN-3 | ✅ #467 | ロケール / 文書 / issue の決着 | WARN-1, WARN-2 ✅ |

### 素材の同一性（`asset-identity-plan.md`）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| AID-1 | ✅ #456 | `AssetId` 型と `MediaAssetEntry` の分離（フォーマット上げ + 移行。`.ravprj` v9） | — |
| AID-2 | ✅ #456 | 参照 3 系統の切り替え | AID-1 ✅ |
| AID-3 | ✅ #460 | インポートの採番と MediaBin の改名 UI（**露出宣言の所有権をここで決める** — 計画書参照） | AID-2 ✅ |
| AID-4 | ✅ #460 | ロケール / 文書 | AID-1〜3 |

### パラメータのグループ（`done/parameter-groups-plan.md`）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PGRP-1 | ✅ #466 | `NodeTemplate::param_groups` と Properties の分割（挙動不変） | — |
| PGRP-2 | ✅ #466 | 組み込みノードへのグループ宣言（6 個以上の 9 テンプレート） | PGRP-1 ✅ |
| PGRP-3 | ✅ #466 | 開閉状態の永続化（`ui_state.json`。版は据え置き） | PGRP-1 ✅ |
| PGRP-4 | ✅ #466 | In ノードのインスタンスグループ（`.ravprj` v12 / journal v11） | PGRP-1 ✅ |
| PGRP-5 | ✅ #468 | ノードエディタのパラメータ値表示トグル（`ui_state.json`） | — |
| PGRP-6 | ✅ #468 | ロケール / 文書 | PGRP-2〜5 ✅ |

### ワークフロー貫通の UX（`refactor-plan-0808.md`）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| UX-1 | 🟡 | 情報の所在表と往復候補の列挙 | — |
| UX-2 | ⬜ | シナリオ 2 本の台本と集計表 | UX-1 |
| UX-3 | ❓ | 測定結果の取り込みと単位化 | UX-2（測定の実施） |
| UX-4 | ✅ #351 | ノード検索を `type_key` でも引く | — |
| UX-5 | ✅ #355 | Timeline のプロパティ行の絞り込み（AE の reveal 一式: U / P / S / R / T / A / L + Alt+U / Alt+E、Shift で追加） | — |
| UX-6 | ✅ #356 | Timeline 上での値スクラブ | — |
| UX-7 | ✅ #357 | プレイヘッド操作（キーへのスナップ、AE 相当のショートカット） | — |
| UX-8 | ✅ #353 | 時間ルーラ（コンポ終端の可視化と BPM グリッド） | — |
| UX-9 | ✅ #362 | ループ範囲とループ再生 | — |
| UX-10 | ✅ #360 | 素材からレイヤーへの経路（自動配置の廃止 / ドラッグ / Outliner） | — |
| UX-11 | ✅ #352 | 再生停止位置と起動時コンポの設定 2 つ | — |
| UX-12 | ✅ | ロケール / 文書（**各単位の PR が自分の分を運んだので新規作業なし**。ja/en のキー集合が一致し、Timeline / MediaBin / Outliner の仕様と `ui-impl-status.md` が追随済みであることを実測で確認） | UX-4〜11 ✅ |

**`UX-4` 以降は `UX-1`〜`UX-3` に依存しない。** 測定は優先順位付けと
見落としの発見に使うもので、実装の前提条件ではない（計画書の決定事項）。

### ノードグラフの可読性（`node-graph-readability-plan.md`）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| NGR-1 | ✅ #347 | 自動整列の計算（ヘッドレス） | — |
| NGR-2 | ✅ #347 | 整列コマンドと undo | NGR-1 |
| NGR-3 | ✅ #347 | `SettingsLayer` の `node_editor` 節と `edge_style` の永続化 | — |
| NGR-4 | 🟡 | 型によるエッジ配色 | — |
| NGR-5 | ⬜ | 上→下フローモード（描画・整列軸・`flow_direction`） | NGR-1, NGR-3 |
| NGR-6 | 🟡 | Reroute ノード | — |
| NGR-7 | 🟡 | エッジへのドロップでノードを挟む | — |
| NGR-8 | ⬜ | ロケール / 文書 | NGR-2〜7 |

**リリース前は `NGR-1`〜`NGR-3` だけ**、`NGR-4` 以降はリリース後
（`roadmap.md` のフェーズ UX）。

### 文脈依存のパラメータ候補と出力型（`contextual-parameter-options-plan.md`）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| CPO-1 | ⬜ | `ParamOptions` と `contextual_options`（`SiblingLayer`） | — |
| CPO-2 | ⬜ | Properties が文脈付きで候補を引く | CPO-1 |
| CPO-3 | ⬜ | `LayerOutputPort` 候補と `port` の Select 化 | CPO-2 |
| CPO-4 | ⬜ | `dependent_port_updates` と `set_params` での適用 | CPO-3 |
| CPO-5 | ⬜ | `layer` の Int → String 移行（フォーマット版 +1） | CPO-2 |
| CPO-6 | ⬜ | Parent ドロップダウンを `ParamOption` へ寄せる | CPO-1 |
| CPO-7 | ⬜ | ロケール / 文書 | CPO-1〜6 |

`MED-APP-29`（`layer.ref` が数値スクラブ、出力型が `port` に追随しない）が
きっかけだが、直す対象は 1 ノードではなく**文脈から候補と型が決まる機構**。
フォーマット版は `CPO-5` 着手時に `CURRENT_FORMAT_VERSION` の次を取る。

### Wrangle とユーザー定義パラメータ（`wrangle-plan.md`）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| WRG-1 | 🟡 | 式言語の複数文とローカル変数（`;` と代入） | — |
| WRG-2 | ⬜ | 属性への書き戻し（`@attr = …` の左辺） | WRG-1 |
| WRG-3 | ⬜ | `attribute.wrangle` ノード | WRG-2 |
| WRG-4 | ⬜ | spare parameter の機構（任意ノードのユーザー定義パラメータ） | — |
| WRG-5 | ⬜ | spare parameter の編集 UI | WRG-4 |
| WRG-6 | ⬜ | spare parameter を式から名前で引く | WRG-3, WRG-4 |
| WRG-7 | ⬜ | ロケール / 文書 | WRG-1〜6 |

**この計画書は丸ごとリリース後。** リリース前に要った前提の `HIGH-30` は
#346 で解消し、`WRG-4` の分岐（spare parameter が既存の露出モデルに乗るか）は
**案 A（2 段）で決着**した（2026-08-09）。着手条件は残っていない。

### 式言語（REQ-CORE-014 / REQ-CORE-015）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| EXPR-1 | ✅ | 式言語コア（字句・AST・定数畳み込み・依存抽出） | #312 |
| EXPR-2 | ✅ | パラメータ式の配線（`ChannelSource::Expression`） | #316 |
| EXPR-3 | ✅ | キャッシュキーと dirty 伝播への統合 | #316 |
| EXPR-4 | ✅ | Properties の式入力 UI | #320 |
| EXPR-5 | ✅ | フィールド式（`field.expression`） | #316 |
| EXPR-6 | ✅ | 属性アクセス（`@attr` 相当） | #320 |
| EXPR-7 | ✅ | 文書更新 | #320 |

### 公開パラメータ宣言（REQ-PROJ-006）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| EXPO-1 | ✅ | 宣言の型と永続化（フォーマット上げ + マイグレーション） | #294 |
| EXPO-2 | ✅ | 束縛の解決と適用 | #315 |
| EXPO-3 | ✅ | 宣言の機械可読な列挙 | #315 |
| EXPO-4 | ✅ | 素材参照の宣言と差し替え | #315 |
| EXPO-5 | ✅ | 宣言の編集 UI | #321 |
| EXPO-6 | ✅ | サブグラフテンプレートで同じ宣言を使う | #321 |
| EXPO-7 | ✅ | 文書更新 | #321 |

### カラーマネジメント（REQ-RENDER-003 / REQ-CORE-009）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| CM-1 | ✅ #363 | 色空間の型と変換関数（伝達関数・原色行列・`.cube`）（HIGH-25） | — |
| CM-2 | ✅ #363 | 素材の入力色空間とデコードの線形化 + `.ravprj` v8（HIGH-25） | CM-1 |
| CM-3 | ✅ #363 | Viewer の表示変換（HIGH-25） | CM-1 |
| CM-4 | ✅ #363 | 書き出しの出力変換（HIGH-25） | CM-1, EXPORT-1 |
| CM-5 | ✅ #363 | 文書更新（骨格） | CM-2, CM-3, CM-4 |
| CM-7 | ✅ #367 | 表示変換を GPU で行う（自前 + ユーザー提供の `.cube`） | CM-5 ✅ |
| CM-6 | ❓ | `ocio-rs` の導入とビルド戦略（**見送り。需要ゲート**） | CM-5 ✅ |
| CM-9 | ⬜ | `.ocio` の読み込みと GPU シェーダ抽出 | CM-6, CM-7 |
| CM-8 | ⬜ | カラー設定 UI（SET-11 を回収）と文書 | CM-9 |

CM-1〜5（自前の固定変換で骨格を作る単位）は #363 でマージ済み。`HIGH-25` を
回収した。作業空間はリニア Rec.709 原色で、プロジェクト単位の切り替えフラグは
持たない（`color-management-plan.md` の決定事項）。

**`CM-6`（`ocio-rs`）は 2026-08-10 に見送りを判断した。** 成熟度が計画時から
変わっておらず（0.2.1 が 4 週間更新なし、star 3、直近のコミットが全部ビルド
検出の修正）、着手条件を日付から「**`.ocio` か ACES が実際に要求されたとき**」へ
変えた。`REQ-RENDER-003` は Must のままなので capability を落としたのでは
なく、払う時期を需要に合わせただけ。バインディングもその時点で選び直す
（`ocio-rs` か自前の `cxx` / `bindgen` か）。

**OCIO を必要としない GPU 表示変換を `CM-7` として切り出した**（旧 `CM-7` の
内容は `CM-9` へ）。依存は `CM-5` だけなので**今すぐ着手できる**。

### プラグインシステム（REQ-PLUGIN-002 / REQ-PLUGIN-004）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PLUG-1 | 🟡 | `ProcessorRegistry` と組み込みの移設 | — |
| PLUG-2 | ⬜ | manifest 形式とスキャン・ロード | PLUG-1, EXPO-1 ✅ |
| PLUG-3 | ⬜ | WGSL シェーダノード | PLUG-2, GPUBK-1 |
| PLUG-4 | ⬜ | プラグインマネージャ UI | PLUG-3 |
| PLUG-5 | ⬜ | WASM ジオメトリノード | PLUG-2 |
| PLUG-6 | ⬜ | 文書更新 | PLUG-4 |

### GPU バックエンド内製化（REQ-INFRA-009）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| GPUBK-1 | ✅ | バインディング記述をバックエンド非依存に | #272 |
| GPUBK-2 | ✅ | 宣言的ディスパッチ API と再利用（MED-GPU-01） | #274 |
| GPUBK-3 | ✅ | `TextureKey` の形式・用途を自前型に | #280 |
| GPUBK-5 | ✅ | ラスタライズとレンダーパスの抽象 | #281 |
| GPUBK-6 | ✅ | リードバックとアップロードの抽象（HIGH-04。旧 GPUCOMP-8） | #282 |
| GPUBK-7 | ✅ | シェーダ変換経路（naga の各バックエンド出力） | #283 |
| GPUBK-8 | ✅ | interop 出口（OFX / HW デコード用） | #287 |
| GPUBK-4 | ✅ | 生ハンドルの公開を停止（façade の仕上げ） | #291 |
| GPUBK-9 | ✅ | デバイス共有の契約と GPUI フォーク方針（旧 GPUCOMP-11） | #296 |
| GPUBK-14 | ✅ | wgpu 直叩きの取り分を測る（GPUBK-10 の判断ゲート） | #295 |
| GPUBK-10 | ❌ | Metal バックエンド（GPUBK-14 の測定で取り分が出ず見送り） | — |
| GPUBK-11 | ❓ | D3D12 バックエンド（D3D12 での実測待ち。Metal の結果は横流ししない） | — |
| GPUBK-12 | ❓ | Vulkan バックエンド（Vulkan での実測待ち。Metal の結果は横流ししない） | — |
| GPUBK-13 | 🟡 | 文書更新（GPUBK-14 の判定を要件・仕様へ反映） | GPUBK-14 ✅ |
| GPUBK-15 | 🟡 | ディスパッチを 1 コンピュートパスに畳む（149.5 µs / 評価） | GPUBK-14 ✅ |
| GPUBK-16 | 🟡 | ブロッキング読み戻しの 1 ms 切り上げを回収（フレームの 3〜6%） | GPUBK-14 ✅ |

### GPU デバイス喪失からの復旧（HIGH-33）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| GPULOSS-1 | ✅ | `ravel-gpu` の device state（epoch + lost）と自前 wgpu device の loss callback、喪失の一度だけのユーザー通知 | ZC-8 ✅, GPUBK-9 ✅ |
| GPULOSS-2 | ✅ | epoch-aware な評価 worker の停止・再生成と cache budget 維持（PR #485） | GPULOSS-1 |
| GPULOSS-3 | 🟡 | GPUI 採用 wgpu device の loss polling・再採用（Linux / FreeBSD / Windows） | GPULOSS-1, GPULOSS-2 |
| GPULOSS-4 | 🟡 | macOS は自前 device の loss で zero-copy を無効化し CPU fallback に留める | GPULOSS-1, GPULOSS-2 |
| GPULOSS-5 | ⬜ | window lifecycle、export、Viewer lease、テスト、実機確認 | GPULOSS-2, GPULOSS-3, GPULOSS-4 |

### OFX ホスト

| 単位 | 状態 | 内容 | 依存 |
|---|---|---|---|
| OFX-0 | 🟡 | 前提の検証と Windows 経路の判断（測定ゲート） | GPUBK-8 ✅, MED-GPU-07 ✅ |
| OFX-1 | ⬜ | `ofx-host` の骨格と CMake ビルド、ヘッダ vendoring | OFX-0 |
| OFX-2 | ⬜ | プロセス管理と IPC 境界（クラッシュ隔離） | OFX-1 |
| OFX-3 | ⬜ | バンドルの走査・ロードと Property / Memory Suite | OFX-2 |
| OFX-4 | ⬜ | Image Effect Suite（CPU レンダー） | OFX-3 |
| OFX-5 | ⬜ | Parameter Suite と Ravel UI への表示 | OFX-4 |
| OFX-6a | ⬜ | `interop` のインポート方向（Metal / D3D12 共通の前提） | OFX-0 |
| OFX-6b | ⬜ | Metal GPU レンダー（macOS） | OFX-4, OFX-6a |
| OFX-7a | ⬜ | **CUDA GPU レンダー（Windows。準 1 級なので後回しにしない）** | OFX-4, OFX-6a, OFX-0 の判定 |
| OFX-7b | ⬜ | OpenCL GPU レンダー（**Experimental**。既定無効、外部テスター検証） | OFX-7a |
| OFX-8 | ⬜ | 未対応 Suite の `kOfxStatErrUnsupported` と縮退の可視化 | OFX-5 |
| OFX-9 | ⬜ | 文書更新（REQ-PLUGIN-001 の訂正を含む） | OFX-8 |

### Align パネル

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| ALIGN-1 | 🟡 | 整列・分布の計算（ヘッドレス） | — |
| ALIGN-2 | ⬜ | パネルと配線 | ALIGN-1, DOCK-8 |
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

### フリードッキング（旧: パネル配置 #181 を吸収）

`panel-placement-plan.md` の PANEL-1〜3 は未着手のまま
`free-pane-docking-plan.md` に supersede。View トグルの解消（#181）は
DOCK-2 が担う。

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| DOCK-1 | ✅ | レイアウトモデル v2（タブ・インスタンス ID・N 窓ツリー） | — |
| DOCK-2 | ✅ | シェル統合と既定スロット挿入（#181 の解消） | DOCK-1 |
| DOCK-3 | ✅ | ravel-dock クレート骨格（静的描画 + gallery） | DOCK-1 |
| DOCK-4 | ✅ | ravel-dock 対話（D&D・エリアメニュー） | DOCK-3 |
| DOCK-5 | ✅ | gpui-ce-ravel フォークパッチ（`set_always_on_top` 等） | — |
| DOCK-6 | ✅ | マルチウィンドウホスト（全窓同型、MED-APP-01 解消） | DOCK-3, DOCK-5 |
| DOCK-7 | ✅ | TitleBar 共通化と AlwaysOnTop ピン | DOCK-6 |
| DOCK-8 | ✅ | カットオーバー（旧 dock 削除、LOW-APP-17 解消） | DOCK-2, DOCK-4, DOCK-6, DOCK-7 |
| DOCK-9 | ✅ | 永続化とカスタムワークスペース（LOW-APP-14 解消） | DOCK-8 |
| DOCK-10 | ✅ | 実機確認と文書更新 | DOCK-9 |

### 属性スプレッドシート

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| SHEET-1 | ✅ | `EvalRequest` の複数ターゲット化（#302） | — |
| SHEET-2 | ✅ | 選択ノードの評価（#448。`SelectedGeometry` グローバルは作らず、既存の scoped ターゲット経路を非オーバーレイへ開いた） | SHEET-1 |
| SHEET-3 | ✅ | パネル本体（`DataTable`）（#450。ソートと列移動は実装せず — delegate の既定実装が空本体なので動かない UI になる） | SHEET-2, DOCK-8 |
| SHEET-4 | ✅ | 実機確認と文書更新（#450。1 万インスタンスのスクロールを実機で計測） | SHEET-3 |

SHEET-1 と SIM-3 と OVL-2 は同じ型（`EvalRequest` / `EvalUpdate`）を触る。
**順序は SHEET-1 が先で決着した**（2026-08-06、#302）。複数ターゲット化が入った
ので、OVL-2 と SIM-3 は新しい形に**相乗りするだけ**になり、同じ概念が 3 回
実装される事態は消えた。`results` は `nodes` と同じ長さ・同じ順序で埋まり、
失敗したターゲットも `Err` としてスロットを保つ（位置引きが成り立つ）。

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
| PART-6 | ⬜ | GPU シミュレーション（**実施と決定**。前提 2 件を満たしてから） | PART-5, GPU-1, GPU-5, **VRAM キャッシュ方式の決着** |

**PART-6 は「実施」と決定した**（2026-07-29）。ただし測定
（`perf-baseline.md`）は**当初の想定と違う根拠**を示している。

- **CPU ステップは 10 万点で rayon 0.2 ms 前後**（60fps 予算の 1〜2%）。
  つまり PART-1〜5 の CPU 経路だけで 10 万点は成立する。
  **PART-6 の価値はステップの高速化ではない。**
- 実際の律速は描画側（10 万点で `flatten` + `upload` = 3.28 ms、
  100 万点で 27.2 ms）。これを消すのは `GPU-1` + `GPU-5` で、
  PART-6 が意味を持つのはその後。
- **VRAM キャッシュ方式の決着は依然ブロッカー**（状態を 300 フレーム
  保持すると 10 万点で約 720 MB。`particle-plan.md` の未解決節）。
  測定はこの問題を解いていない。GPU 状態の読み戻しが
  10 万点で ≈1.35 ms（固定レイテンシ律速）だと分かったので
  「スクラブは CPU / 再生は GPU」は
  帯域面では成立する、という材料が増えただけ。

**3D（`3D-4` 以降）は最初から GPU 実装**（三角形レンダラ + CPU 参照経路）で、
今回の測定で変わったのは `GPU-1` を待たないという判断の裏付けだけ。

### エフェクトライブラリ（REQ-MOGRAPH-005）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| FX-1 | 🟡 | カラー調整とカラーグレーディング（トーンカーブは PARAM-1 の型を使う） | PARAM-1（トーンカーブのみ） |
| FX-2 | 🟡 | ブラー / シャープ / ディストーション | — |
| FX-3 | 🟡 | 生成とスタイライズ（グラデーションは PARAM-1 の `Ramp` を使う） | PARAM-1（グラデーションのみ） |
| FX-3b | 🟡 | `comp.solid` / `comp.fill` / `comp.tint` / `comp.alpha` | — |
| FX-4 | 🟡 | トランスフォーム拡張と合成 | — |
| FX-5 | ⬜ | 時間系（`SCOPE-2` の時間シフト経路に載る） | FX-1〜4, SCOPE-2 |
| FX-6 | ⬜ | レジストリ / ロケール / 文書 | FX-1〜5, FX-3b |

FX-3b は raster 側に**生成ノードが 1 つも無い**ことへの対応
（`crates/ravel-nodes/src/comp/` は merge / opacity / transform のみ、
`rasterize` は Geometry 入力が必須なので単色平面すら作れない）。
`comp.fill` はアルファを保った RGB 置換で、ジオメトリ側の `style.fill`
（属性を書くノード）とは別概念。

### GPU 常駐ジオメトリ

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| GPU-0 | ✅ | **Phase 0 測定** → 判断は**実施** | — |
| GPU-1 | ⬜ | `GpuGeometry` 型と転送 | — |
| GPU-5 | ⬜ | **`rasterize` が常駐ジオメトリを直接読む（CPU 展開の除去）** | GPU-1 |
| GPU-3 | ⬜ | 生成ノードの GPU 化 | GPU-1 |
| GPU-2 | ⬜ | フィールドの WGSL 評価 | GPU-1 |
| GPU-4 | ⬜ | 文書更新 | GPU-1〜3, GPU-5 |

**GPU-0 は測定済み（2026-07-29、`perf-baseline.md`「ジオメトリ評価
スケーリング baseline」）。判断は実施。** 10 万インスタンスの end-to-end が
直列 18.24 ms（予算 16.6 ms）。

**優先順が計画の想定と変わった。** 支配的なのは CPU 評価でもアップロードでも
なく、`rasterize` が毎フレーム CPU でインスタンスを展開するコスト
（10 万で 8.75 ms = CPU 側の 77%）。これを消す単位が上表の **GPU-5** で、
今回の測定で追加した。行の並びが実施順（GPU-1 → GPU-5 → GPU-3 → GPU-2）。
CPU フィールド評価は 10 万で 1.17 ms しかないので、GPU-2 が効くのは
50 万要素超。

`MOD-5` への依存は外れた（フィールドチェーンを手組みで代用して測定済み）。

### メディア / オーディオ（進行中）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| A3-1 | ✅ | epoch 付き再生キュー | #207 |
| A3-2 | ✅ | SetTrack の非同期準備とリサンプラ終端 | #207 |
| A3-3 | ✅ | sample-accurate audio decode | #207 |
| A3-4 | ✅ | encoder の channel layout と固定 frame 化 | #207 |
| A3-5 | ✅ | 出力デバイス能力の採用 | #207 |
| A3-6 | ✅ | 文書と完了ゲート | #207 |
| A4-1 | ✅ | 出力レート音声のアセットキャッシュ | #212 |
| A4-2 | ✅ | Composition 終端の Pause 公開 | #212 |
| A4-3 | ✅ | 音声準備状態と失敗の可視化 | #212 |
| A4-4 | ✅ | 文書更新と完了ゲート | #212 |
| MEDIA-1〜5 | ✅ | アセットモデル / media ノード / インポート / MediaBin / サムネイル | #167, #173, #176, #177, #169 |
| MEDIA-6 | ✅ #469 | Properties + 再リンク（`Save As` の参照付け替えを含む） | — |
| MEDIA-7 | ✅ #470 | オフライン表示 + 文書（Outliner / Timeline のレイヤー行に印） | MEDIA-6 ✅ |
| AUDIO-1〜4 | ✅ | データモデル / ミキサ / 再生配線 / 動画音声 | #172, #168, #174, #178 |
| AUDIO-5 | 🟡 | 波形表示 | — |
| AUDIO-6 | 🟡 | 解析ノード（RMS / ピーク） | — |
| AUDIO-7 | ⬜ | バンクのタグ・試聴 | AUDIO-5 |

### 3D シーン（REQ-3D）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| 3D-1a | ✅ | **`P` の次元許容**（Vec2 \| Vec3。`as_vec2` 58 箇所の規約） | — |
| 3D-1b | ✅ | **`Primitive::Mesh` の追加と網羅規約**（match 53 箇所。レンダラなし） | — |
| 3D-2 | ✅ | `orient` / `scale3` / `N` 標準属性と回転ユーティリティ | 3D-1a |
| 3D-3 | ✅ | `Scene` データ型とカメラ | 3D-1a, 3D-1b |
| 3D-4 | 🟡 | 三角形レンダラと `scene.render` | 3D-3 |
| 3D-5 | ⬜ | 基本プリミティブ（box / sphere / cylinder / plane） | 3D-4 |
| 3D-6 | ⬜ | 3D 複製（`scatter.*` の 3D 対応） | 3D-2, 3D-5 |
| 3D-7 | ⬜ | ライティング | 3D-4 |
| 3D-8 | ⬜ | 押し出しとベベル | 3D-4, TYPE-*, PATH-0b |
| 3D-9 | ⬜ | モデル読み込み（glTF / OBJ） | 3D-4 |
| 3D-10 | ⬜ | レジストリ / ロケール / 文書 | 3D-1〜9 |

**3D-1a / 3D-1b は早く入れるほど安い**。`as_vec2` 呼び出しが 58 箇所 /
12 ファイル、`Primitive::Path` の match が 53 箇所 / 7 ファイルで、
OPS-1〜13 / PATH-1〜6 / TYPE-* が入ると合わせて 100 箇所を大きく超える。
レンダラ（3D-4）は後から足しても既存ノードに影響しない。3D-1a は
`Positions` で `P` の読み出しを一本化し、分類表を仕様書に確定させた
（`docs/specifications/procedural-geometry.md`）ので、以降のノードは
その表を引くだけで済む。

**1a と 1b は独立した軸**なので分けてある。組み合わせは 4 通りすべて意味を
持つ（Vec3 の `P` + Path = 3D の折れ線、Vec2 の `P` + Mesh = 平面の
三角形分割）。1 単位にまとめるとレビューできない大きさになる。

**主要ユースケースはプリミティブ + 複製**（C4D の MoGraph 相当）なので、
3D-5 / 3D-6 が実用性の中心。押し出し（3D-8）は TYPE-* 依存で後回しでよい。

`GPU-1`（GPU 常駐ジオメトリ）には**依存しない**。判断ゲートに 3D を人質に
取らせないためで、`GPU-0` が「実施」で確定した後もこの判断は変えない。
毎フレームの頂点アップロードは数十万頂点まで成立する（6.5〜7.9 GB/s 実測、
`perf-baseline.md`）。条件は**静的メッシュを毎フレーム上げ直さない**こと。

### 画像インスタンス（FrameBuffer の複製）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| IMG-1 | ✅ | `SceneContent::Image` の退場（挙動不変。REQ-3D-001 本文の修正を含む）（#309） | — |
| IMG-2 | ✅ | `InstanceSource` への一般化（`ravel-core`、挙動不変）（#418） | — |
| IMG-3 | ✅ | `geometry.from_image` ノード（FrameBuffer → Geometry）（#418） | IMG-2 |
| IMG-4 | ✅ | `rasterize` のテクスチャ経路（CPU 参照）（#426） | IMG-2, IMG-3 |
| IMG-5 | ✅ | `rasterize` のテクスチャ経路（GPU）（#430） | IMG-4 |
| IMG-6 | ✅ | レジストリ / ロケール / 文書（#430） | IMG-1〜5 |

**`IMG-1` だけ先に入れる。** `scene.render`（`3D-4`）が未着手で
`SceneContent::Image` / `FlatContent::Image` の消費者がテスト以外ゼロなので、
今なら挙動不変で畳める。`3D-4` / `3D-5` / `3D-7` が両方の上に積んでから
消すと跳ね上がる（`roadmap.md` の基準 3）。

**`IMG-2` 以降はフェーズ C4 の後**（決定 9）だった。ゲートは「書き出しが
開くまで他の投資が回収されない」という基準 0 の判断であって、先行単位の
完了ではない（`IMG-2` に `IMG-1` への技術的な依存は無い）。
**フェーズ C4 が完了した（2026-08-13）のでこの順序ゲートは解け、`IMG-2` は
着手可能になった。** `IMG-3` 以降は `IMG-2` への技術的な依存が残るので
依存待ちのまま。

`GPU-5` は「`instance_sources` は CPU 側メタデータのまま」を前提にしており、
テクスチャハンドルはその前提に収まらない。`CACHE-3` は画像を抱えた
`Geometry` を VRAM 層に計上する必要がある。どちらも
`done/image-instancing-plan.md` の「未解決の依存」に書いてある。

### ジオメトリ破砕（Cell Fracture）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| FRAC-1 | ✅ | 多角形の三角形分割器（`earcut` を採用、#306） | PATH-0b |
| FRAC-2 | 🟡 | `geometry.cell_fracture`（2D。三角形分割 + 半平面クリップ） | FRAC-1 |
| FRAC-3 | 🟡 | `geometry.cell_fracture_3d`（Mesh を平面で bisect） | FRAC-1, 3D-1a, 3D-1b |
| FRAC-4 | ⬜ | アルゴリズム選択式と実行時列挙（boolean 経路） | FRAC-2, PATH-1 |
| FRAC-5 | ⬜ | レジストリ / ロケール / 文書 | FRAC-2〜4 |

**boolean には依存しない。** Voronoi セルは凸なので、三角形分割 + 半平面
クリップで厳密に実装できる。boolean は FRAC-4 で**任意選択のアルゴリズム**
として足す。ただし三角形分割器は必要なので **FRAC-1 は `PATH-0b`
（三角形分割器の採用判断）に依存する** — `PATH-0a`（boolean の方針）とは独立。既定は依存なしの経路で固定し、使えない選択は明示エラーにする
（ビルド差で絵が変わる事故を防ぐ）。

## 計画外の課題

| 項目 | 内容 |
|---|---|
| AddNode の検索 UI | ノード追加を Blender 風の検索パレットにする。fork の gpui-component に `searchable_list`（`SearchableListDelegate` / `SearchableGroup`）があり、`NodeCategory` 別のグルーピングもそのまま乗る。**副作用として `add_node_menu_model` の毎 render 再構築が消える**（`issues/high/` の再描画問題の一因）。単一パネルの機能追加なので設計ゲート対象外 |
| #181 | View トグルがプリセット配置依存 → `free-pane-docking-plan.md`（DOCK-2）で対応 |
| グローバル設定層の配線 | `settings.rs` の 4 層マージと TOML 入出力は実装済みだが、global 層が `global_settings_path()` から読み書きされていない（`resolved_settings` の呼び出し元がテストのみ）。レイアウト永続化の前提だったが、`free-pane-docking-plan.md`（DOCK-9）はレイアウトを専用 TOML に分離して保存するため、この配線には依存しない |
| `decode_full_audio` の確保量 | 常に 128MB 相当の `Vec::with_capacity` |
| 実コーデック音声テスト | `ffmpeg` feature が既定オフのため **CI で走らない** |
| Lua / 式 | REQ-CODE-001 / REQ-PLUGIN-003。REQ-MOGRAPH-001 と REQ-CORE-010 の受入条件が 1 つずつこれ待ちで残る |
| トランジション | REQ-MOGRAPH-005 の受入条件だがタイムライン側の仕事。計画なし |
| REQ-DATA 全体 | CSV/JSON → Table → 属性バインディング。データ駆動インフォグラフィックスの柱ごと欠落。計画なし |
| REQ-RENDER-002 Write ノード | 評価純粋性とディスクキャッシュ設計の問題。`done/render-export-plan.md` の非対象 |
| Fuse / パス自己交差解消 | 空間分割構造が要る |
| ビート検出 | FFT 見送りの延長 |
| レイヤー制約（look-at / パス追従） | ジオメトリ側は VEC-3 で解決するが、レイヤー殻には無い |

### 実機フィードバック由来の機能要望（2026-08-08）

実機を触って挙がった要望のうち、**バグではなく機能追加**のもの。`issues/` は
実装単位でない項目を持たない規約なので、計画が付くまでここに置く。
（バグとして起票したものは `issues/` 側にある — `HIGH-27`〜`HIGH-29`、
`MED-APP-26`〜`MED-APP-30`、`LOW-APP-23`）

**2026-08-08 の grill で 18 件に計画が付いた。** 下の第 1 表がその行き先で、
中身は各計画書が正（ここには単位 ID だけを残す）。第 2 表がまだ計画の
無い残り 5 件。

#### 計画書が引き受けたもの

| 項目 | 担当計画 | 単位 |
|---|---|---|
| ノード検索を type 名でも引く | `refactor-plan-0808.md` | UX-4 |
| AE の reveal ショートカット一式 | `refactor-plan-0808.md` | UX-5 |
| Timeline に値のスクラブ | `refactor-plan-0808.md` | UX-6 |
| Shift + プレイヘッド移動でキーへスナップ | `refactor-plan-0808.md` | UX-7 |
| AE 相当の Timeline ショートカット | `refactor-plan-0808.md` | UX-7 |
| コンポの Duration を可視化 | `refactor-plan-0808.md` | UX-8 |
| Timeline の BPM グリッド | `refactor-plan-0808.md` | UX-8 |
| ループ範囲の指定とループ再生 | `refactor-plan-0808.md` | UX-9 |
| AssetImport 時の自動配置をやめる | `refactor-plan-0808.md` | UX-10 |
| Outliner からのレイヤー追加 | `refactor-plan-0808.md` | UX-10 |
| 停止したら再生開始位置へ戻す | `refactor-plan-0808.md` | UX-11 |
| 起動時にコンポを作るかの on/off | `refactor-plan-0808.md` | UX-11 |
| ノードの自動整列 | `node-graph-readability-plan.md` | NGR-1, NGR-2 |
| 型でエッジの色を変える | `node-graph-readability-plan.md` | NGR-4 |
| NodeEditor の上→下フローモード | `node-graph-readability-plan.md` | NGR-5 |
| Reroute ノード | `node-graph-readability-plan.md` | NGR-6 |
| エッジの間にノードを挟む | `node-graph-readability-plan.md` | NGR-7 |
| Wrangle 相当 | `wrangle-plan.md` | WRG-1〜7 |

`Wrangle 相当` の元の記述は「式言語（`EXPR-*`）と WGSL シェーダノード
（`PLUG-3`）の交差点」だったが、**`PLUG-3` は要らない**というのが調査の
結論。根拠は `wrangle-plan.md` の「調査で分かったこと」。

#### まだ計画の無いもの

| 項目 | 内容 |
|---|---|
| スカラ → 全成分同値の Vec | broadcast。`VEC-6`（`constant.vec2/3/4`）の隣 |
| Vec の全軸同時操作 | Shift 押下で全成分を一緒に動かす。`MED-APP-20`（成分ラベルとリンクトグル）と同じ範囲 |
| `field.apply` の domain / target 補完 | 自由文字列なので候補を出す。`MED-APP-29` の Select 化と同型 |
| Noise フィールドの evolution / flow | 時間で連続変化させるシード軸 |
| ベジエ Ellipse と Arc | 内半径を持つ Arc（ドーナツ）を含む |

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
