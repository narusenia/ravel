# low — 軽微なバグ・磨き込み・小さな負債

影響が限定的、または発現条件が狭い項目。個別 issue にせずまとめて管理する。
着手時は該当セクションを切り出して個別ファイル化してよい。

---

## ravel-core

**LOW-CORE-01 | bug | `deterministic_node_id` の ID 切り詰めと `deterministic_edge_id` の非単射ハッシュ**
`crates/ravel-core/src/composition/compile.rs:51-79`
エンコードは `comp << 32 | layer << 8 | role` でマスク無し、デコードは layer を 24bit にマスクする。
`LayerId ≥ 2^24`（ID はグローバル単調増加で再利用されない）や `CompId ≥ 2^32` で
別レイヤーの合成ノードと無言でエイリアスし、`set_document` の無効化が誤り、
コンパイルが duplicate-node で失敗する。
`deterministic_edge_id` は `source*0x9E3779B9 ^ target` で、
異なるエッジ対が衝突して `compile_composition` が偽の `DuplicateEdge` で失敗しうる。
（コンパイル済みグラフは ephemeral — `project_state.rs:865` の `compile_composition(comp, Graph::new())` —
なので合成 ID が ID ウォーターマークに漏れないことは確認済み。）
→ エンコード側の範囲を debug_assert し、違反時は明示的に失敗 / ログ。
エッジ ID はハッシュではなく両端点の構造から衝突しない packed 方式で導出。

**LOW-CORE-02 | debt | シェルの transform / opacity チャンネルが `NodeOutput` / `Expression` を受理して黙って 0.0 になる**
`crates/ravel-core/src/animation/channel.rs:96-100`（消費側 `composition/transform.rs:101-125`）
`layer_matrix` / `world_matrix` は `AnimationChannel::evaluate` 経由で評価するが、
そこで `NodeOutput` と `Expression` はプレースホルダ `DEFAULT_VALUE`(0.0) を返す。
一方モデルはこれらを積極的にサポートしている —
`Layer::duplicate_with_fresh_ids`（`composition/mod.rs:243-259`）は
`transform` / `opacity` / `audio.gain` の `NodeOutput` バインディングを丁寧に再マップし、
`Evaluator::set_document` もシェル状態として扱う。
位置をノード出力にバインドしたレイヤーは診断なしで原点に描画される（スケールなら 0 に潰れる）。
ネットワーク内のパラメータ経路は `NodeOutput` を正しく解決する（`eval.rs:1260-1301`）。
シェル経路だけが無言のスタブ。
→ 実装までは検証時にシェルチャンネルの `NodeOutput` / `Expression` を拒否する。
または `resolve_source` 同様に評価器経由で解決する。最低限プレースホルダ到達時に警告ログ。

**LOW-CORE-03 | perf | Bézier キーフレームサンプリングが常に 60 回の二分探索を回す**
`crates/ravel-core/src/animation/interpolation.rs:62-76`
`solve_t_for_x` はサンプルごとに固定 60 回二分する（約 240 flops の3次評価）。
f32 出力では約 24 回で仮数を使い切り、文書化された要件 1e-4 なら約 14 回で足りる。
シェルトランスフォームはレイヤーごと・フレームごとに最大7つの Bézier チャンネルをサンプルし、
加えて全アニメーションノードパラメータも通る。
→ `hi - lo < 1e-6` で早期終了（または Newton + 二分フォールバック）。60 は病的ケースの上限にする。

**LOW-CORE-04 | bug | `a_load_time_pin_removal_is_logged` が稀に落ちる（機構未特定）**
`crates/ravel-core/src/composition/mod.rs`（`warnings_from` と
`a_load_time_pin_removal_is_logged`）
`mise run check` の workspace テストで **1 回だけ**、捕まえた警告文が空で落ちた。
その後 `ravel-core --lib` 全体を 6 回、`composition::` を 3 回、
`mise run check` を計 4 回回して再現していない。
機構の候補は潰してある: `sync_subnet_pins` は単一スレッド（`rayon` も
`thread::spawn` も無い）、`warnings_from` は `tracing::subscriber::with_default`
＝スレッドローカルなので並列テストに奪われない、警告に warn-once の
ガードは無い、`ravel-core` のテストは環境変数を触らない。
→ **CI で赤を見たらこの項目に追記する。**再現の筋道が立つまで直せない。
1 回の観測だけで消すには惜しいので記録だけ残す（次に見た人が同じ 20 分を
使わないため）。

---

## ravel-gpu / ravel-nodes

**LOW-GPU-02 | bug | `merge.wgsl` の最終 mix が直線アルファで動作する**
`crates/ravel-nodes/src/shaders/merge.wgsl:56`
`result = mix(b, result, params.mix_val)` が直線アルファの色を線形補間する。
`b` と `result` のアルファが異なる場合（透明な黒 B の下に不透明な result など）、
透明側の無意味な RGB で暗くなる。
CPU の adjustment merge（`comp/merge.rs:190-200`）は等価な mix の前に正しく premultiply しており、
2つの mix セマンティクスが乖離している。
→ 両オペランドを premultiply → mix → un-premultiply（`merge_adjustment` に合わせる）。
[medium/gpu-nodes.md](../medium/gpu-nodes.md) の MED-GPU-02 と同時に対処。

**LOW-GPU-03 | perf | `transparent()` が呼び出しごとに全解像度のゼロ f32 フレームを確保**
`crates/ravel-nodes/src/comp/mod.rs:62-64`, `:121-129`（`media.rs:126` も同様）
範囲外レイヤー、merge の入力欠落、オフラインメディアがそれぞれ
`FrameBuffer::new_zeroed`（4K で約 33MB）をノードごと・フレームごとに確保する。
`FrameBuffer.data` は `Arc<[f32]>` なので、解像度ごとに共有ゼロフレームを持てば clone は無料。
→ 解像度ごとに1枚キャッシュ（thread-local か hooks 上）して Arc を clone。

**LOW-GPU-04 | debt | TexturePool の帳簿: `by_key` エントリが削除されない、LRU 走査が O(n)、usage 完全一致キーで共有が制限される**
`crates/ravel-gpu/src/texture_pool.rs:137-167`, `:240-293`
`by_key` は distinct な `TextureKey` ごとのエントリを永久保持する（空になった Vec も drop されない）
ため、多様な解像度に触れる長時間セッションでマップが無制限に増える（小さいがリーク）。
`LruBudget::remove` は線形走査、`evict_overflow` は最悪 O(n²)。
プーリングは usage フラグの完全一致を要求するため、Rgba16Float RENDER_ATTACHMENT のラスタターゲットと
rw コンピュートテクスチャは同サイズでも別サブプールになる（仕様通りだが 512MiB 予算の調整時に留意）。
（再利用自体は正しく動作することを確認済み: acquire は LIFO で pop、
acquire/release/evict で LRU / idle / by_key が整合、`GpuFrameBuffer` は最後の drop で
リースをちょうど1回返す。）
→ 空になった `by_key` エントリを削除。プールが大きくなるなら `LruBudget` を順序付きマップか
intrusive list に変更。

**LOW-GPU-05 | bug | `perf_baseline` が評価 0 回でも「1 回」として平均を出す**
`crates/ravel-nodes/examples/perf_baseline.rs`（`evaluations.max(1)` / `evals.max(1)`、複数箇所）
`evaluations` は**成功した**評価だけを数えるが、完了チャネルは失敗した結果も報告する。
最終評価が失敗すると `evaluations` が 0 になり、`.max(1)` が
「1 回完了した」ものとして submits / パス数の平均を割る。
つまり**全部失敗した実行が、1 回成功した実行と同じ形の数字を出す** —
0 除算を避けるための `.max(1)` が、避けたついでに嘘の分母を作っている。
影響は計測ハーネスの出力だけで製品コードには無いが、
**この数字は `perf-baseline.md` に記録され性能判断の根拠になる**ので、
黙って妥当に見える値が出るのは害。
（`GPUBK-14` / `GPUBK-9` の計測で `.max(1)` の行に触れた際に発見。
既存の挙動で、それらの変更が入れた退行ではない。当該計測は評価が成功していたので
記録された数字自体は影響を受けていない。）
→ 成功 0 回のときは平均を `N/A` として出す。または完了した評価を全部数えて
分母の意味をラベルに書く（`/ completed evaluation` を `/ successful evaluation` と
区別する）。どちらでも「0 を 1 と書かない」ことが要点。

---

## ravel-media / ravel-audio

**LOW-MED-01 | bug | `hw_get_format` のフォールバックが先頭の提示フォーマットを返す（別の HW フォーマットの可能性）**
`crates/ravel-media/src/decoder.rs:103-122`
コメントは「HW フォーマットが提示されていない → 最初の SW フォーマットを返す」と書くが、
コードは `*pix_fmts`（リスト先頭）を返す。混在リストでは通常ハードウェアフォーマット
（ターゲットが見つからなかったので別種）になる。
デコーダはソフトウェアに降格せず open に失敗する。特殊なコーデック / ドライバ組み合わせでのみ到達。
→ `av_pix_fmt_desc_get` のフラグ等でハードウェアでない最初のエントリを探して返す。

**LOW-AUD-02 | perf | 音声チャンクの収集バッファが要求長ぶんの容量を常に前借りする**
`crates/ravel-media/src/decoder.rs:610-618`, `crates/ravel-app/src/audio/mixdown.rs:300-305`
`AudioChunkCollector::new` は `sample_count × channels` の容量を確保する。
`decode_full_audio` は上限（`MAX_DECODE_BYTES` 相当のフレーム数）を `sample_count` に渡すため、
**3 秒のファイルでも 128MiB を確保する**。`AudioBuffer` の `Arc<[f32]>` 化で
実サイズへコピーし直すので恒久的な浪費ではないが、ピークメモリと全長コピーは残る。
→ 上限は打ち切り条件としてのみ使い、容量はストリーム長の見積り（`duration × rate`）で確保する。
`HIGH-23` の準備経路の作り直しと同時に触るのが安い。

**LOW-AUD-03 | debt | オフラインミックスダウンの「1 アセット 1 デコード」が失敗時に成立しない**
`crates/ravel-audio/src/offline.rs:113-115`（doc）・`:139-157`
`mix_range` の `decoded` マップは**成功したデコードだけ**を覚える。
同じ asset + stream を 2 レイヤーが使い、その素材が上限超過なら
`prepare` が**レイヤーごとに全長デコードを試みる**（オフライン・不読なら
デコードはしないが `prepare` は 2 回走る）。doc コメントは
「どれだけのレイヤーが使っても 1 回」と書いており、事実と違う。
書き出し 1 回ぶんなので実害は小さいが、上限超過素材では 2 倍の
デコードコストになる。
→ 失敗も `HashMap<CacheKey, Result<…>>` として覚えるか、doc を実装に合わせる。

**LOW-MED-02 | debt | 意図的に !Send な FFmpeg ラッパーに対する包括的 `unsafe impl Send`**
`crates/ravel-media/src/encoder.rs:50`, `crates/ravel-media/src/hwaccel/device.rs:30`
`unsafe impl Send for FfmpegEncoder` は構造体の現在および将来の全フィールドを覆い、
ffmpeg-the-third が意図的に付けた !Send マーカーを上書きする。
安全性の論拠（「単一所有者の逐次ライター」）は型レベルの保証ではなく使用上の慣習。
現状は健全だが、本当にスレッド親和性のあるフィールドを追加しても（あるいは `Arc` で共有しても）
コンパイルが通ってしまう。
→ impl は残しつつ範囲を狭める。生の FFI ハンドルを専用 newtype に包み、
そこに unsafe impl と不変条件コメントを置いて、外側の構造体は構造的に Send を導出する。

---

## UI レンダリング（軽微）

**LOW-UI-01 | perf | ノードエディタがノードごと・フレームごとにシェイプテキストとパラメータ文字列を整形、エッジは未カリング**
`crates/ravel-app/src/node_editor/painting.rs:157-239`（`paint_edges` にカリング無し）、
`:556`, `:605-655`（ポート / パラメータごとの `shape_line` + `format!`）
`paint_nodes` は画面外ノードをカリングしている（`:353`、良好）が、
`paint_edges` は完全に画面外のエッジも含め全エッジのベジエ `PathBuilder` を構築する。
`paint_single_node` は描画ごとにパラメータ値文字列を再整形（`format!("{v:.2}")` 等）し、
ラベル・各ポート・各パラメータで `shape_line` を呼ぶ。
GPUI の行レイアウトキャッシュが再シェイプを吸収するが、文字列確保とルックアップは
ノードごと・フレームごとに繰り返される。
→ エッジを端点 AABB でカリング。整形済みパラメータ文字列をノードリビジョンでキャッシュ。

**LOW-UI-02 | perf | `NodeEvalTimings` の HashMap が評価更新ごとに2回 clone される**
`crates/ravel-app/src/project_state.rs:908-916`, `crates/ravel-app/src/panels/node_editor.rs:1638-1641`
評価結果ごとに `HashMap<NodeId, Duration>` グローバル全体を clone → extend → 再設定し、
ノードエディタの render がもう一度 clone する。
→ グローバルを `im::HashMap` か `Arc<HashMap>` にする。ノードエディタ非表示時は更新をスキップ。

**LOW-UI-03 | perf | Viewer の render が毎フレーム選択 bbox / パスオーバーレイをドキュメントから再計算**
`crates/ravel-app/src/panels/viewer.rs:1528-1577`
各 `render()` が Document を clone（`im` なので安価）し、`selection_comp_rects`、
レイヤー bbox の union（レイヤーごとに `world_matrix`）、パスオーバーレイを再計算する。
paint クロージャの外に正しく置かれているが、再生中はフレームごとに再レンダーされるため
選択が変わっていなくてもこのジオメトリ計算が 30〜60 回 / 秒走る。
→ (ドキュメントリビジョン, プレイヘッドフレーム, 選択) をキーに計算結果をキャッシュ。

**LOW-UI-04 | bug | Windows の wgpu レンダラが初回サーフェスサイズに 2^31 を要求する（上流）**
gpui-ce フォーク `gpui_wgpu/src/wgpu_renderer.rs`（Ravel 側にコードは無い）
`ZC-7` の実機確認（PR #390）で、ウィンドウ生成のたびに次が出る:
`Requested surface size (2147483648, 2147483648) exceeds maximum texture dimension 16384.`
`2147483648` は 2^31 ちょうどで、実サイズではなく**未初期化ないし
プレースホルダの値**が伝わっている。レンダラが 16384 にクランプするため
描画は正常で、実害は確認されていない（実機でテキスト・パネル・Viewer とも
問題なし）。**Windows で `gpui_platform` の `wgpu` feature を有効にした
場合のみ**現れる（macOS の Metal レンダラ、Linux の既定経路では出ない）。
→ フォーク側の問題なので上流 gpui-ce に投げるのが筋。クランプが効いている
うちは実害が無いため優先度は低いが、**ウィンドウサイズ計算の初期化順序に
本当のバグがあるなら他の症状も出る**ので、`ZC-7` ステップ 2 で Windows を
触るときに一度追うこと。

---

## ravel-app / ravel-ui（軽微なバグ）

**LOW-APP-02 | bug | クリックによる前面移動（z 変更）がコミットされず、無関係な undo ステップに混入する**
`crates/ravel-app/src/panels/node_editor.rs:1744`
`raised_to_front` がマウスダウン時に表示グラフを変更する。
単なるクリックではコミットされないので refresh で元に戻る、
または次の無関係な `commit_graph` に相乗りする。
→ ドラッグが実際に動くまで raise を遅延させる。または z が変わったならマウスアップでコミット。

**LOW-APP-03 | bug | Shift + ドラッグのボックス選択が既存選択を拡張せず置換する**
`crates/ravel-app/src/panels/node_editor.rs:1760-1764`, `:1923-1949`
バンド開始に Shift を要求するのに、publish するのはボックス内容のみで、
Shift セマンティクスが保つはずの既存選択を捨てる。
→ 開始時点でキャプチャした選択 ∪ ボックス内容を publish。

**LOW-APP-04 | bug | Timeline のラバーバンドが負のコンポジションフレームにある不可視キーフレームを選択する**
`crates/ravel-app/src/panels/timeline.rs:1744-1791`（描画側は `:2696` でカリング、ヒットテストはしない）
Delete でユーザーが見たことのないキーを削除する。
→ バンドの `min_x` を 0 でクランプ、または描画側のカリングを適用。

**LOW-APP-05 | bug | フレームレートが minor ステップで割り切れないとルーラーの major tick / ラベルが消える**
`crates/ravel-app/src/panels/timeline.rs:4524-4535`（`curve_grid_canvas` も同様）
24fps で minor=5 なら major は 120 フレームごと、23.976 で minor=240 / major=1439 なら実質描かれない。
→ major を minor の倍数に丸める、または major を別ループで反復。

**LOW-APP-06 | bug | コンポジション切替 / 短縮時にプレイヘッドがクランプされない**
`crates/ravel-app/src/panels/timeline.rs:354-401`, `:2342-2346`
短いコンポジションへ切り替える（または設定 / undo で短縮する）と、
次のスクラブまでミラーされたプレイヘッドが終端を超えたまま残る。
→ `sync_from_project` でクランプ。

**LOW-APP-09 | bug | `format_duration` が分境界で `0:60.0` を出す**
`crates/ravel-app/src/panels/media_bin.rs:481-485`
→ 先に 0.1 秒単位へ丸めてから分へ桁上げする。

**LOW-APP-10 | bug | ジェスチャー終了時のコミットが、外部から削除された対象に no-op undo ステップを記録する**
`crates/ravel-app/src/panels/viewer.rs:850-873`, `:226-252`,
`crates/ravel-app/src/panels/node_editor.rs:2089-2098`
Viewer の stale ジェスチャークリーンアップに `shape_drag` が漏れている。
コンテキストメニューの削除は全削除が失敗してもコミットする。
→ 対象が解決できない場合はコミットをスキップ。`selection_sub` のクリーンアップに `shape_drag` を追加。

---

## ravel-app / ravel-ui（軽微な負債）

**LOW-APP-12 | debt | パネル間で重複したヘルパー（乖離リスク）**
- `field_label` のロケールフォールバック: `panels/properties.rs:75-87` = `composition_form.rs:33-41`
- `hsla_from_rgba`: `properties.rs:500-502` = `composition_form.rs:226-233`
- `with_node_editor`（クロスウィンドウ遅延ヘルパー）: `properties.rs:905-922` = `outliner.rs:555-572`（バイト単位で同一）
- シェイプ / ペンのノード生成 + 配線 約40行: `viewer.rs:2218-2263` vs `:2311-2355`（+ 2つのレイヤーラッパー）
- パネルの `duplicate_layer` が `ravel_ui::document::duplicate_layers` を再実装: `timeline.rs:585-609`

→ それぞれ共有モジュール / ヘルパーへ引き上げる。

**LOW-APP-13 | debt | 文字列型のノード / パラメータキーがリテラルで散在**
`crates/ravel-app/src/panels/viewer.rs:2319`, `:2389` vs `node_editor.rs:53`
（`CUSTOM_PATH_TYPE_KEY` が存在するのに viewer がリテラルを繰り返す）。
`"points"`, `"closed"`, `"center_x"/"center_y"`（`ravel-ui/src/document.rs:335-338` にも）、
`"rasterize"`, `"output"` に共有定数が無い — ravel-core でのリネームが無言で bounds / 配線を壊す。
→ ravel-core または共通モジュールに共有キー定数を置く。

**LOW-APP-16 | debt | Timeline の壊れやすい panic 箇所**
- `crates/ravel-app/src/panels/timeline.rs:3940` — `.expect("builtin layer command")`。
  現状は安全だが、コマンド配列が `layer_template_key` と乖離するとメニュー展開時に panic。
  `filter_map` にする
- `timeline.rs:1356` — `clamp(0, origin_out-1)` は `out_frame == 0` が起きると panic。
  現在の書き込み側はすべて ≥1 を保つが `Layer::with_time` は 0 を受理する。`min`/`max` を使う
- `timeline.rs:1423` — dead な `let _ = changed;`

**LOW-APP-27 | bug | `ui_state.json` に載る UI 状態を変えてもプロジェクトが dirty にならず、保存確認も出ずに失われる**
`crates/ravel-app/src/project_state.rs:659`（`is_dirty` は `revision != saved_revision` だけを見る）
`ui_state.json` に永続化される状態 — Timeline の BPM グリッド、コンポジションごとの
ループ範囲、Properties で畳んだパラメータグループ（`PGRP-3`）、ノード本体の
パラメータ値表示（`PGRP-5`）— はどれも Global を書くだけで `revision` を
動かさない。したがって:
- 保存済みプロジェクトで畳んだ / トグルしたあと**明示的に Save せずに**
  終了・File ▸ New・File ▸ Open すると、**保存確認が出ない**
- 次に開くと既定値に戻っている（利用者からは「覚えてくれない」に見える）
**永続化そのものは動いている**（明示的に Save → Load で残る）。欠けているのは
「UI 状態も未保存の変更である」という扱い。
→ 判断が要る: (a) UI 状態の変更も dirty にする（`revision` とは別のフラグでも
よい。保存確認の条件に足す）、(b) UI 状態は明示保存の対象外と割り切って
文書にそう書く。**(a) なら「保存しますか」が UI 設定の変更でも出る**ので、
どちらが望ましいかはプロダクトの判断。
**備考**: #468（`PGRP-5`）の独立レビューで指摘された。`PGRP-3` から在る形で、
`bpm_grid` / `loop_ranges` まで遡る。

**LOW-APP-28 | debt | 数を埋め込むロケール文字列に単数形が無く「1 audio streams」と出る**
`assets/locales/{en,ja}.toml`（`properties.media.audio_streams` /
`kind_sequence` / `duration_frames` ほか `{count}` を持つキー）、
`crates/ravel-ui/src/properties/media_asset.rs`（`probe_fields`）
`ravel_i18n::translate` はキーを引いて `{count}` を置換するだけで、
**複数形の選択機構が無い**。英語では「1 audio streams」「1 frames」のように
出る（日本語は影響なし）。`duration_frames` は以前から同じ形なので、
**新しいキーだけ単数形を足すと規約が 2 つになる**。
→ 判断が要る: (a) `translate` に複数形の選択を足す（ICU MessageFormat 相当か、
`*.one` / `*.other` の 2 キー規約）、(b) 数を含む文言を「Audio streams: 1」の
形に寄せて単数複数の問題を回避する。
**(b) は文言の作り直しだが機構を増やさない**。どちらもロケール全体に効く
判断なので、キー単位で場当たりに直さないこと。
**備考**: #469（`MEDIA-6`）の CodeRabbit レビューで指摘された。

**LOW-APP-26 | debt | 非アクティブなコンプのノード行 / レイヤー行はシングルクリックが無反応で、理由が画面に出ない**
`crates/ravel-app/src/panels/outliner.rs:698`（`on_row_click`）
`active = active_composition == row.comp()` で、ノード行とレイヤー行の
シングルクリックは `else if active` の中にある。**非アクティブなコンプの行を
1 回クリックしても何も起きない**（行のハイライトすら変わらない）。
これは設計どおりで、理由は関数の doc にある（`LayerSelection.comp ==
ActiveComposition` を保つため）。問題は**そう見えないこと** — 「壊れている」と
読めるので、ダブルクリックならコンプを切り替えて選択できることに気づけない。
ヘッドレステスト（`a_node_row_selects_the_node_in_its_layer_network`）は
アクティブなコンプの行しか押さないので、**この経路はテストにも無い**。
→ (a) 非アクティブなコンプの行を「今は選べない」と見て分かるようにする、
または (b) シングルクリックでコンプを切り替えてから選択する。
どちらも 1 行の判断が要るので、まず**無反応であることのテスト**を足して
現状を固定するだけでも良い。

**LOW-APP-23 | bug | ノードの評価時間の表示がノード直下に出て、下のノードに被る**
`crates/ravel-app/src/node_editor/painting.rs:495-508`
評価時間の読み出しを**ノードごとに、ノードの直下**（`wy + sh + 2`）へ描く。
ノードを縦に詰めると下のノードに重なる。再生していないときも出続ける。
→ 位置を固定（キャンバス左上）にするか、少なくとも下のノードと重なるときは
出さない。再生中だけ出す案も実機報告に含まれている。

**LOW-APP-25 | bug | 環境設定のキーバインド一覧が Windows / Linux でも `Cmd+` と表示する**
`crates/ravel-ui/src/keybindings/mod.rs:190-198`（`impl Display for KeyChord`）
`KeyChord::command` は「macOS では Cmd、Windows / Linux では Ctrl」と型の
docstring が定義しているのに、`Display` はどのプラットフォームでも `Cmd+` と
書く。アセットの表記でもあるので**この形自体は正しい**（`default.toml` は
`Cmd+S` と書く）が、環境設定のキーバインド一覧はこの `Display` をそのまま
画面に出しているため、**Windows のユーザーは押せないキーの名前を読む**。
gpui へ渡す側は `chord_to_gpui_string` が `secondary-` へ変換して解決済み
（`fix/windows-primary-modifier`）。残っているのは表示だけ。
→ 保存形（`Display`）と表示形を分ける。表示側で `cfg!(target_os = "macos")` に
応じて `Cmd+` / `Ctrl+` を出す。**`Display` を触ると資産の書式が変わる**ので、
そちらは動かさないこと。

## 参考: 監査で問題なしと確認された箇所

- 永続化のマイグレーション連鎖 v1→v4 は防御的でテスト十分
  （破損入力で panic しない、`TooNew` を拒否、`ui_state.json` は優雅に劣化、
  アセットパスは正しく相対化）
- `DocumentStore` の undo / redo / revert セマンティクスと `ProjectState` の
  save/load における generation・revision フェンシングは正しい
  （順序が乱れた保存キューイング、stale なロードの破棄を含む）
- グローバルなベアキーバインド（Space/K/矢印）は入力中も安全 —
  gpui-ce fork の `prefer_character_input` が、テキストを受け付けるフォーカス入力がある間
  バインディングをスキップする（vendored `window.rs` で確認）
- キーバインド衝突検出、コード解析、command↔action マクロテーブルは網羅的で乖離テストあり
- ロケールファイル en/ja はキー同一（235/235）、`t!` のフォールバック連鎖は健全
- トレースレコーダは有界
- 上記 LOW-APP-16 と [medium/app-shell.md](../medium/app-shell.md) の MED-APP-12 以外、
  パネル内に到達可能な production の `unwrap` / `expect` / インデックス panic は見つからなかった

---

**LOW-APP-18 | debt | `ViewStates<T>` が呼び出し元ゼロの公開 API**
`crates/ravel-ui/src/lib.rs`（再公開）
多重インスタンスのビュー状態はインスタンスごとに別エンティティであることで
成立していて、この型は誰も使っていない。`LOW-APP-14` の
`WindowPlacement`（配線されていない契約）と同じ形の負債。
→ 配線するか削除する。

**LOW-APP-19 | debt | detach した窓がドロップ位置ではなく画面中央に開く**
`crates/ravel-app/src/window_host.rs`（`open` / `window_bounds_for`）
`DockEvent::TabDetachRequested` はリリース位置を画面座標で運んでいるが、
`open` は**復元された** placement しか尊重しないので、タブをどこで離しても
新しい窓は 640×480 で中央に出る。掴んだものが手元から飛ぶ挙動になる。
→ 新規 detach ではドロップ位置を初期 placement に使う。

**LOW-APP-20 | debt | 設定フィールドの書き込み配線がテストから叩けない**
`crates/ravel-app/src/settings_dialog.rs`（`fields_for` の各フィールド）
`gpui_component` の `SettingField::set_value` は `pub(crate)` で、公開トレイト
`AnySettingField` にも出ていない（`is_resettable` / `reset` は出ている）。
このため **「その dropdown を選ぶとその設定が書かれる」配線だけが無カバレッジ**で、
フィールドが別の設定へ書いていても気づけない。reset 側は
`AnySettingField::reset` を実際に呼ぶテストで配線ごと固定できているので、
非対称はライブラリ側にある。書き込みのロジック自体は `app_settings::update` を
直接呼ぶテストが覆っている。
→ fork（`narusenia/gpui-component`）の `AnySettingField` に値の取得 / 設定を
足して配線をテストする。**pinned git dependency の変更**なので着手前に要確認
（`.agents/rules/rust.md`）。`SET-12`（キーバインドの割り当て編集）が同じ
seam を必要とするので、その前が自然なタイミング。

**LOW-APP-21 | bug | `is_default` を持つテーマファイルは 2 回目以降のホットリロードで自分自身がスキップされる**
`assets/themes/ravel.json`（`"Ravel Light"` / `"Ravel Dark"` の `is_default: true`）
gpui-component の `ThemeRegistry::reload()` は `themes` を clear → `default_themes` を
先に挿入 → **ファイル由来のうち同名のものを `continue` でスキップ**という順序。
`is_default: true` のテーマは初回 reload で `default_themes` に昇格するため、
2 回目の reload では自分の名前が既に `themes` にあり、**ファイルの変更が捨てられる**。
テーマファイルを編集しながら見た目を詰める作業が 1 回しか効かない形になる。
**コードを読んで辿った筋道で、実機では未確認。**
→ まず実機で再現を確認する。再現するなら (a) 資産から `is_default` を外す
（既定テーマが gpui-component のものに戻るので、`app_settings` 側の
フォールバックが同梱テーマを名前で拾えることを確認したうえで）か、
(b) fork の `reload()` の順序を直す。開発時のみの影響で、リリース版の挙動には
出ない。

**LOW-APP-22 | debt | rustdoc の警告が誰にも見られていない（ワークスペース 30 件）**
`mise run check` は fmt / パターン lint / clippy / テストを回すが **`cargo doc` を
回さない**。pre-commit も CI も同じなので、rustdoc の警告は積まれる一方になる。

実測（2026-08-05、`cargo doc --workspace --no-deps`）:

| クレート | 件数 |
|---|---|
| `ravel-app` | 13 |
| `ravel-core` | 9 |
| `ravel-media` | 4 |
| `ravel-nodes` | 2 |
| `ravel-ui` | 2 |
| `ravel-gpu` | 0（`MED-GPU-07` の PR で潰した） |

内訳は 4 種類で、上 2 つが 27 件を占める。**リンク切れ**
（`unresolved link to AppShell` / `NodeProcessor` / `Document` /
`EvalService` など、型名だけ書いて import もパスも無いもの）、
**private 化した項目への public doc からのリンク**、
`write` がマクロと関数で曖昧（`project/atomic_write.rs` に 2 件）、
冗長な明示リンク先（`widgets/param_curve_editor.rs` に 1 件）。
後者は `GPUBK-4`（#291）が façade を閉じたときに 8 件生んでいて、
**誰も気づかないまま main に入った**のがこの issue の直接の動機。

害は「壊れたリンクが出荷される」ことだけでなく、**リファクタで可視性を
変えたときに doc が置き去りになったことを機械的に検出できない**こと。

→ 判断が要る。選択肢は 3 つ:

1. **`cargo doc` を `mise run check` に足す**（`RUSTDOCFLAGS="-D warnings"`）。
   再発は止まるが、**先に 30 件を潰す必要がある**うえ CI 時間が増える
   （ワークスペースの doc ビルドは実測で clippy と同程度）
2. **CI のみに足す**（pre-commit には入れない）。ローカルの往復は増えないが
   落ちるのが遅い
3. **やらない。** 定期的に手で見る

`ravel-gpu` の 0 件は `MED-GPU-07` のついでに潰したもので、他は手つかず。
1 を採るなら残り 30 件の掃除が前提作業になる。**件数が少ないうちに決めた方が
安い**（`ravel-app` が 13 件で最多、パネルが増えるほど伸びる）。
