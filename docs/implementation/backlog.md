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

最終更新: 2026-07-31

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
| INSP-2 | チャンネル単独表示（R / G / B / A） | `viewer-inspection-plan.md` |
| INSP-3 | ピクセル値の読み取り | `viewer-inspection-plan.md` |
| TOOLX-1 | Hand / Zoom ツールの実装（MED-APP-15） | `viewer-tool-extensions-plan.md` |
| TOOLX-2 | 矩形選択 | `viewer-tool-extensions-plan.md` |
| SNAP-1 | 既存要素へのスナップ（他レイヤー / コンプ枠 / セーフエリア） | `viewer-snap-guides-plan.md` |
| SHELL-5 | `parent` の設定 UI（Properties の Parent ドロップダウン） | `layer-shell-wiring-plan.md` |
| SCOPE-2 | 時間シフト経路（FX-5 の土台） | `evaluation-scope-plan.md` |
| SCOPE-3 | `geometry.iterate`（ピース単位反復） | `evaluation-scope-plan.md` |
| SIM-1 | `StatefulProcessor` と sim キャッシュの骨格 | `stateful-eval-plan.md` |
| MOD-3 | 駆動ソース `field.time` / `field.constant` | `per-instance-modulation-plan.md` |
| MOD-4 | `attribute.delete`（属性列の削除） | `per-instance-modulation-plan.md` |
| VEC-1 | 二項合成の多相化 | `vector-field-plan.md` |
| GPUCOMP-8 | リードバック実装の改善（HIGH-04） | `gpu-compositing-plan.md` |
| SET-8 | キャッシュ設定 | `settings-screen-plan.md` |
| ALIGN-1 | 整列・分布の計算（ヘッドレス） | `align-panel-plan.md` |
| SHEET-1 | `EvalRequest` の複数ターゲット化 | `attribute-spreadsheet-plan.md` |
| OPS-1 | `geometry.blast`（要素削除） | `geometry-ops-plan.md` |
| OPS-2 | `geometry.sort`（並べ替え） | `geometry-ops-plan.md` |
| OPS-3 | `geometry.resample` | `geometry-ops-plan.md` |
| OPS-4 | `geometry.measure` | `geometry-ops-plan.md` |
| OPS-5 | `geometry.switch` / `geometry.null` | `geometry-ops-plan.md` |
| OPS-6 | `geometry.group_index`（index で要素指定） | `geometry-ops-plan.md` |
| OPS-7 | `geometry.repeat`（トランスフォームリピータ） | `geometry-ops-plan.md` |
| OPS-8 | デフォーマ（bend / twist / taper） | `geometry-ops-plan.md` |
| STYLE-1 | 塗り・線のスタイル属性読み出し | `style-attributes-plan.md` |
| STYLE-5 | `field.apply` の属性自動作成 + Color 既定マスク | `style-attributes-plan.md` |
| OPS-11 | `shape.line` / `shape.grid` | `geometry-ops-plan.md` |
| OPS-12 | `geometry.connect`（要素を結ぶ） | `geometry-ops-plan.md` |
| OPS-13 | `attribute.curveu`（パスパラメータ） | `geometry-ops-plan.md` |
| VEC-6 | `constant.vec2` / `vec3` / `vec4`（VEC-5 完了で着手可能） | `vector-field-plan.md` |
| NETIF-1 | 出力ポートの再インデックス API | `network-interface-editing-plan.md` |
| INFO-1 | `InvalidationHint::Shell`（挙動不変） | `scene-info-nodes-plan.md` |
| OVL-5 | `ParamRole` とマニピュレータ | `viewer-overlay-manipulator-plan.md` |
| OVL-7 | レイヤー殻のマニピュレータ + HUD + 親子リンク線 | `viewer-overlay-manipulator-plan.md` |
| PARAM-7 | `math.curve`（値ドメインの curve remap） | `properties-parameter-editors-plan.md` |
| 3D-2 | `orient` / `scale3` / `N` 標準属性と回転ユーティリティ | `3d-scene-plan.md` |
| 3D-3 | `Scene` データ型とカメラ | `3d-scene-plan.md` |
| FX-3b | `comp.solid` / `comp.fill` / `comp.tint` / `comp.alpha` | `effects-library-plan.md` |
| SHELL-1 | `time_remap` の配線 | `layer-shell-wiring-plan.md` |
| SHELL-2 | `track_matte` の配線 | `layer-shell-wiring-plan.md` |
| CACHE-5 | フレームキャッシュ層（comp 単位の無効化） | `cache-plan.md` |
| CACHE-8 | 共有デコードフレームキャッシュ（HIGH-16 / MED-MED-02） | `cache-plan.md` |
| BLUR-3 | 品質段階 `EvalContext.quality` | `motion-blur-plan.md` |
| SET-1 | 設定の適用経路と言語（UI なし。日本語を到達可能にする） | `settings-screen-plan.md` |
| SET-2 | 設定ダイアログの骨組み | `settings-screen-plan.md` |
| PATH-0a | ブーリアンの実装方針評価（依存判断） | `path-ops-plan.md` |
| PATH-0b | 三角形分割器の採用判断（FRAC-1 / 3D-8 のゲート） | `path-ops-plan.md` |
| EXPORT-0 | 永続化を GUI 非依存クレートへ抽出 | `render-export-plan.md` |
| EXPORT-1 | エンコーダ抽象と実行時列挙 | `render-export-plan.md` |
| EXPR-1 | 式言語コア（字句・AST・定数畳み込み・依存抽出） | `expression-language-plan.md` |
| GPUBK-1 | バインディング記述をバックエンド非依存に | `gpu-backend-plan.md` |
| PLUG-1 | `ProcessorRegistry` と組み込みの移設 | `plugin-system-plan.md` |
| EXPO-1 | 宣言の型と永続化（`NETIF-2` 完了で着手可能） | `exposed-parameters-plan.md` |
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
保留した MED-UI-06（同じ変更が2経路で届く重複 sync）は実装単位になっていないので
この表には無く、**`roadmap.md` のフェーズ C3「応答性の残り」**がクラスタとして
順序を決めている（個票は `issues/`）。

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
| GPUCOMP-8 | 🟡 | リードバック実装の改善（HIGH-04） | GPUCOMP-7 |
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

### キャッシュ（REQ-CORE-006）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| CACHE-1 | ✅ | `FrameBuffer` の精度多相化（規約のみ。`as_f32` アクセサ + lint） | — |
| CACHE-2 | ✅ | `CacheIdentity` の抽出と時間基準化（旧 BLUR-2、HIGH-03） | — |
| CACHE-3 | ✅ #230 | `CacheBudget` と退避（MED-CORE-06 / 07） | CACHE-1, CACHE-2 |
| CACHE-4 | ✅ #227 | スコープ無効化の粒度修正（MED-CORE-02） | CACHE-2 |
| CACHE-5 | 🟡 | フレームキャッシュ層（comp 単位の無効化） | CACHE-3, GPUCOMP-7 |
| CACHE-6 | ⬜ | Timeline のキャッシュ帯と `cache_stats` | CACHE-5 |
| CACHE-7 | ⬜ | 無効化を時間範囲に絞る | CACHE-5 |
| CACHE-8 | 🟡 | 共有デコードフレームキャッシュ（HIGH-16 / MED-MED-02） | CACHE-3 |
| CACHE-9 | ⬜ | 先読み（投機充填） | CACHE-5 |
| CACHE-10 | ⬜ | 文書更新 | CACHE-7 |
| CACHE-Y | ❓ | per-pixel ループの format 汎用化（実測後。他は依存しない） | CACHE-1 |
| CACHE-11 | ❓ | ディスク層（測定ゲート） | CACHE-5 |

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
（MED-CORE-02）。**`settings.toml` の `[cache]` は解決されるが実行時には
届かない** — 走行中の予算へ流す配線は `SET-8`。

### 設定画面と設定の適用（REQ-PROJ-004）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| SET-1 | 🟡 | 設定の適用経路と言語（UI なし。MED-APP-10 の中核） | — |
| SET-2 | 🟡 | 設定ダイアログの骨組み（`gpui_component::setting::Settings`） | — |
| SET-3 | ⬜ | 外観（テーマモード / テーマ選択） | SET-1, SET-2 |
| SET-4 | ⬜ | 言語の切り替え UI | SET-1, SET-2 |
| SET-5 | ⬜ | キーバインドのユーザー上書きと一覧（LOW-APP-15） | SET-2 |
| SET-6 | ⬜ | プロジェクト設定画面（既定フレームレート） | SET-2 |
| SET-7 | ⬜ | 文書更新 | SET-6 |
| SET-8 | 🟡 | キャッシュ設定 | CACHE-3 |
| SET-9 | ❓ | 自動保存（間隔 / 無効化） | REQ-PROJ-002 のタイマー実装 |
| SET-10 | ❓ | プロキシ設定 | プロキシ生成の実装 |
| SET-11 | ❓ | カラー設定（OCIO） | カラー管理の実装 |
| SET-12 | ❓ | キーバインドの割り当て編集 | SET-5（別計画に切り出す判断もあり） |
| SET-13 | ❓ | 設定の import / export | 項目が揃ってから |
| SET-14 | ❓ | UI スケーリング | 調査（パネルが `Theme.font_size` を尊重しているか） |
| SET-15 | ❓ | 色覚多様性テーマ / アニメーション削減 | テーマ資産の追加とアニメーション箇所の棚卸し |

**「出す項目 = 実際に効く項目」が規約**なので、SET-8 以降は前提機能の
マージ後に着手する（`settings-screen-plan.md`）。SET-1 は UI なしで完結し、
これだけで `ja.toml`（235 キー）が到達可能になる。

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
| OPS-11 | 🟡 | `shape.line` / `shape.grid`（表の「生成 ✅」は誤りだった） | — |
| OPS-12 | 🟡 | `geometry.connect`（要素をベジエ/直線で結ぶ。Add SOP 相当） | — |
| OPS-13 | 🟡 | `attribute.curveu`（パスパラメータ `u` の予約と書き込み） | — |
| OPS-10 | ⬜ | レジストリ / ロケール / 文書 | OPS-1〜9, OPS-11〜13 |

### 塗り・線のスタイル属性化

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| STYLE-1 | 🟡 | スタイル属性の読み出し（CPU / GPU） | — |
| STYLE-2 | ⬜ | `style.fill` / `style.stroke` ノード | STYLE-1 |
| STYLE-3 | ⬜ | ダッシュ・キャップ・ジョイン | STYLE-1 |
| STYLE-4 | ⬜ | 変調との結合検証と文書 | STYLE-2, MOD-1 |
| STYLE-5 | 🟡 | `field.apply` の属性自動作成 + Color 既定マスクを `rgb` へ | — |
| STYLE-6 | ⬜ | `field.ramp`（位置 → 色のランプ） | STYLE-5, VEC-1 |

STYLE-5 の「Color 既定マスクを `rgb`」は**既定値の変更**。現状スカラー
フィールドは Color の全 4 成分に broadcast され、明度と同時にアルファも動く
（`crates/ravel-core/src/geometry/field.rs:686-688`）。

### ベクタ場

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| VEC-1 | 🟡 | 二項合成の多相化（**Color / Vec4 を含む**） | MOD-2 |
| VEC-2 | ⬜ | 変換ノード（length / component / compose / angle） | VEC-1 |
| VEC-3 | ⬜ | ベクタ場（direction_to / curl_noise / gradient / radial） | VEC-2 |
| VEC-7a | ✅ | `vector.construct.vec2` / `vec3` / `vec4`（値ドメイン。VEC-5 の移行が挿入する） | — |
| VEC-5 | ✅ | Vec パラメータの正規化（`_x`/`_y` → `Channel2` / `Channel3`、`Channel3`→VEC3 ポート、`attribute.set` の型駆動 `value` と再型付け、format v5 マイグレーション） | VEC-7a |
| VEC-6 | 🟡 | `constant.vec2` / `vec3` / `vec4` | VEC-5 |
| VEC-7b | ⬜ | `vector.split` / `swizzle`（値ドメイン） | VEC-6, NETIF-1 |
| VEC-8 | ⬜ | `vector.length` / `normalize` / `dot` / `cross`（値ドメイン） | VEC-6 |
| VEC-4 | ⬜ | look-at・フロー場のゴールデン検証と文書 | VEC-3, VEC-5〜8 |

**VEC-7a を VEC-5 より先に置いているのは循環を切るため**。VEC-5 の移行は
「`center_x` と `center_y` の両方に別ノードが繋がっている旧ファイル」で
`vector.construct` を挿入する必要がある。`construct` は Scalar 入力と Vec
出力だけで成立し `constant.vec*` を要らないので、単位 7 から切り出せる。
アリティは `type` パラメータではなく `type_key` で分けた（ポート型が
ノードインスタンスに保存されるため。計画書の単位 7 に根拠を記載）。

**VEC-5 は 2 つの計画のゲート**で、両方の前提が満たされた。組み込みノードの
Vec は `Channel2` / `Channel3` の 1 パラメータになったので、
`viewer-overlay-manipulator-plan.md` の `ParamRole`（1 パラメータ = 1 つの意味）
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
| NETIF-6 | 🟡 | Collapse / Extract | NETIF-5 |
| NETIF-7 | ⬜ | レジストリ / ロケール / 文書 | NETIF-1〜6 |

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

残るのは `NETIF-6`（Collapse / Extract）と `NETIF-7`（掃き寄せ）。
`NETIF-6` の前提となるピン同期は `NETIF-5` で入っている。

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
| INSP-2 | 🟡 | チャンネル単独表示（R / G / B / A） | INSP-1 |
| INSP-3 | 🟡 | ピクセル値の読み取り | OVL-1 |
| INSP-4 | ⬜ | 再生とキャッシュの状態表示 | （キャッシュ表示のみ CACHE-6） |
| INSP-5 | ❓ | スコープ 4 種の引き取り判断 | — |

INSP-1 は**設定できるのに効かない**フィールドの解消なので、他の検査機能より
先に入れる（`roadmap.md` フェーズ A5）。表示オプションは `.ravprj` にも
`ui_state.json` にも保存しない（セッション内のパネル状態）。

### Viewer のスナップとガイド

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| SNAP-1 | 🟡 | 既存要素へのスナップ（他レイヤー / コンプ枠 / セーフエリア） | OVL-1 |
| SNAP-2 | ⬜ | 定規とユーザーガイド（`Composition` へ追加フィールド、format v4 据え置き） | SNAP-1 |
| SNAP-3 | ⬜ | ロケールと文書 | SNAP-1, SNAP-2 |

SNAP-2 は永続化を触るが**追加フィールド + `serde(default)`** なので
format version もマイグレーションも増えない（`Layer.audio` の前例）。
したがって基準 1（移行コストが時間で増える）は効かず、後回ししてもコストは上がらない。

### Viewer ツールの拡張

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| TOOLX-1 | 🟡 | Hand / Zoom ツールの実装（MED-APP-15） | — |
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
| OVL-2 | ⬜ | オーバーレイ用の評価要求（multi-target に相乗り） | OVL-1, SHEET-1 |
| OVL-3 | ⬜ | Geometry オーバーレイ + `shape_node_bounds` の廃止 | OVL-2 |
| OVL-4 | ⬜ | Field オーバーレイ | OVL-2 |
| OVL-5 | 🟡 | `ParamRole` とマニピュレータ | OVL-1, VEC-5 |
| OVL-7 | 🟡 | レイヤー殻のマニピュレータ（scale / rotation / anchor）+ HUD + 親子リンク線 | OVL-1 |
| OVL-8 | ⬜ | ジオメトリ属性の空間可視化（矢印 / index / group） | OVL-3 |
| OVL-9 | ⬜ | モーションパス（軌跡表示 + キー位置のドラッグ。空間ベジェは持たない） | OVL-1, OVL-7 |
| OVL-6 | ⬜ | ロケール / 文書 | OVL-1〜5, OVL-7〜9 |

OVL-2 は `EvalRequest` を触る 3 つ目の計画。独自経路は作らず
`attribute-spreadsheet-plan.md` 単位 1 の multi-target 化に乗る。

OVL-7 は選択 bbox の 8 ハンドルを**初めて機能させる**単位（現状は描画だけで
スケール・回転のジェスチャーが存在しない）。`VEC-5` には依存しない — 殻は
最初から `[AnimationChannel; 2]`。`done/pointer-feedback-plan.md` が保留した
`Resize*` / 回転カーソルもこの単位で入る。

### Properties の複合パラメータエディタ

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PARAM-1 | ✅ | `ParameterValue::Curve` と文字列からのマイグレーション（format v6） | — |
| PARAM-2 | ✅ | カーブエディタのインライン展開（アコーディオン） | PARAM-1 |
| PARAM-3 | ⬜ | `ParameterValue::Ramp` と `field.ramp` | PARAM-1, STYLE-6 |
| PARAM-4 | ⬜ | グラデーションエディタのインライン展開 | PARAM-3 |
| PARAM-5 | ✅ | カーブエディタの表示範囲を Timeline と共有（`widgets/curve_view.rs`。Timeline 側のホイール縦ズームは `MED-APP-17` に残る） | PARAM-2 |
| PARAM-7 | 🟡 | `math.curve`（値ドメインの curve remap） | PARAM-2 |
| PARAM-8 | ⬜ | `color.ramp`（値ドメインのカラーランプ。Blender ColorRamp 相当） | PARAM-4 |
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
| PATH-0b | 🟡 | **三角形分割器の採用判断**（`earcut` / 自前） | — |
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
| SHELL-5 | 🟡 | `parent` の設定 UI（Properties の Parent ドロップダウン、循環候補を除外） | — |
| SHELL-4 | ⬜ | 文書更新 | SHELL-3, SHELL-5 |

SHELL-5 は他の 3 つと**向きが逆の取り残し** — `parent` は評価では効くのに
設定 UI がどこにも無い（基準 4「評価はできるが編集できない」）。Viewer の
親子リンク線は `OVL-7` が持つので、この単位は設定手段だけ。

### モーションブラー（REQ-RENDER-004）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| BLUR-1 | ✅ | アニメーションチャネルの連続時間化 | #187 |
| BLUR-2 | — | **`cache-plan.md` の CACHE-2 に統合** | — |
| BLUR-3 | 🟡 | 品質段階 `EvalContext.quality` | CACHE-2 |
| BLUR-4 | ⬜ | `comp.motion_blur` と殻フィールド | BLUR-3 |
| BLUR-5 | ⬜ | 文書更新 | BLUR-4 |

BLUR-2（キャッシュ有効性を `time` 基準へ）は `cache-plan.md` の CACHE-2 に
統合し、そこで実装した。同じ有効判定を 2 計画で別々に書き換えると衝突するため。
これが無いと BLUR-3〜5 は「実装したのにブレない」形で静かに壊れていた
（キャッシュが整数 frame を見ているため 2 サンプル目以降がヒットする）。
BLUR-3 の `quality` は CACHE-2 の `CacheIdentity` に軸として足す。

### 書き出しと CLI（REQ-RENDER-001 / REQ-RENDER-005）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| EXPORT-0 | 🟡 | 永続化を GUI 非依存クレートへ抽出 | — |
| EXPORT-1 | 🟡 | エンコーダ抽象と実行時列挙 | — |
| EXPORT-2 | ⬜ | レンダーワーカーとキュー | EXPORT-0, EXPORT-1, BLUR-3 |
| EXPORT-3 | ⬜ | **CLI（`ravel render`）** | EXPORT-2, EXPO-3 |
| EXPORT-4 | ⬜ | 音声のミックスダウンと多重化 | EXPORT-2 |
| EXPORT-5 | ⬜ | 書き出し UI | EXPORT-3, DOCK-8 |
| EXPORT-6 | ⬜ | 文書更新 | EXPORT-5 |

### 式言語（REQ-CORE-014 / REQ-CORE-015）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| EXPR-1 | 🟡 | 式言語コア（字句・AST・定数畳み込み・依存抽出） | — |
| EXPR-2 | ⬜ | パラメータ式の配線（`ChannelSource::Expression`） | EXPR-1 |
| EXPR-3 | ⬜ | キャッシュキーと dirty 伝播への統合 | EXPR-2 |
| EXPR-4 | ⬜ | Properties の式入力 UI | EXPR-2 |
| EXPR-5 | ⬜ | フィールド式（`field.expression`） | EXPR-1 |
| EXPR-6 | ⬜ | 属性アクセス（`@attr` 相当） | EXPR-5 |
| EXPR-7 | ⬜ | 文書更新 | EXPR-4, EXPR-6 |

### 公開パラメータ宣言（REQ-PROJ-006）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| EXPO-1 | 🟡 | 宣言の型と永続化（フォーマット上げ + マイグレーション） | NETIF-2 ✅ |
| EXPO-2 | ⬜ | 束縛の解決と適用 | EXPO-1 |
| EXPO-3 | ⬜ | 宣言の機械可読な列挙 | EXPO-1 |
| EXPO-4 | ⬜ | 素材参照の宣言と差し替え | EXPO-2 |
| EXPO-5 | ⬜ | 宣言の編集 UI | EXPO-2 |
| EXPO-6 | ⬜ | サブグラフテンプレートで同じ宣言を使う | EXPO-2, NETIF-6 |
| EXPO-7 | ⬜ | 文書更新 | EXPO-5 |

### プラグインシステム（REQ-PLUGIN-002 / REQ-PLUGIN-004）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| PLUG-1 | 🟡 | `ProcessorRegistry` と組み込みの移設 | — |
| PLUG-2 | ⬜ | manifest 形式とスキャン・ロード | PLUG-1, EXPO-1 |
| PLUG-3 | ⬜ | WGSL シェーダノード | PLUG-2, GPUBK-1 |
| PLUG-4 | ⬜ | プラグインマネージャ UI | PLUG-3 |
| PLUG-5 | ⬜ | WASM ジオメトリノード | PLUG-2 |
| PLUG-6 | ⬜ | 文書更新 | PLUG-4 |

### GPU バックエンド内製化（REQ-INFRA-009）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| GPUBK-1 | 🟡 | バインディング記述をバックエンド非依存に | — |
| GPUBK-2 | ⬜ | 宣言的ディスパッチ API と再利用（MED-GPU-01） | GPUBK-1 |
| GPUBK-3 | ⬜ | `TextureKey` の形式・用途を自前型に | GPUBK-1 |
| GPUBK-4 | ⬜ | 生ハンドルの公開を停止 | GPUBK-2, GPUBK-3 |
| GPUBK-5 | ⬜ | ラスタライズとレンダーパスの抽象 | GPUBK-4 |
| GPUBK-6 | ⬜ | リードバックとアップロードの抽象（HIGH-04。旧 GPUCOMP-8） | GPUBK-4 |
| GPUBK-7 | ⬜ | シェーダ変換経路（naga の各バックエンド出力） | GPUBK-4 |
| GPUBK-8 | ⬜ | interop 出口（OFX / HW デコード用） | GPUBK-4 |
| GPUBK-9 | ⬜ | デバイス共有の契約と GPUI フォーク方針（旧 GPUCOMP-11） | GPUBK-4 |
| GPUBK-10 | ⬜ | Metal バックエンド | GPUBK-5〜7 |
| GPUBK-11 | ⬜ | D3D12 バックエンド | GPUBK-10 |
| GPUBK-12 | ⬜ | Vulkan バックエンド | GPUBK-10 |
| GPUBK-13 | ⬜ | 文書更新 | GPUBK-10 |

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
| SHEET-1 | 🟡 | `EvalRequest` の複数ターゲット化 | — |
| SHEET-2 | ⬜ | 選択ノード評価と `SelectedGeometry` グローバル | SHEET-1 |
| SHEET-3 | ⬜ | パネル本体（`DataTable`） | SHEET-2, DOCK-8 |
| SHEET-4 | ⬜ | 実機確認と文書更新 | SHEET-3 |

SHEET-1 と SIM-3 と OVL-2 は同じ型（`EvalRequest` / `EvalUpdate`）を触る。
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
| MEDIA-6 | 🟡 | Properties + 再リンク | — |
| MEDIA-7 | ⬜ | オフライン表示 + 文書 | MEDIA-6 |
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

### ジオメトリ破砕（Cell Fracture）

| ID | 状態 | 単位 | 依存 |
|---|---|---|---|
| FRAC-1 | ⬜ | 多角形の三角形分割器（`earcut` 採用 or 自前） | PATH-0b |
| FRAC-2 | ⬜ | `geometry.cell_fracture`（2D。三角形分割 + 半平面クリップ） | FRAC-1 |
| FRAC-3 | ⬜ | `geometry.cell_fracture_3d`（Mesh を平面で bisect） | FRAC-1, 3D-1a, 3D-1b |
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
