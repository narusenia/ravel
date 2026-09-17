# medium — ravel-app / ravel-ui / ravel-cli（シェル・パネル・状態管理・ヘッドレス経路）

---

## MED-APP-02 | bug | タイムライン終端の自動一時停止が publish されない（再生ボタンが戻らず、音声も止まらない）

**該当**: `crates/ravel-app/src/playback.rs:220-236`, `:437-472`

通常のティック間隔では最終フレームが `playing=true` で publish される。
次のティックで `frame_from` 内部が自動一時停止するが、フレームが変わらないため
`tick_with` が `None` を返し、`publish` / `forward_transport(false)` が走らない。
再生 / 一時停止アイコンは「再生中」のまま（notify されない）、
音声エンジンには Pause が送られない。
（一時停止が publish されるのはフレームがまだ動く late-tick 経路のみ。）

**修正方針**: フレーム移動が無くても `is_playing()` が false に遷移した時点で
更新を emit する（またはティックループで明示的に publish / forward する）。

---

## MED-APP-09 | bug | 音声トラック構築がライブ編集ごとに UI スレッドで無制限の作業を行う

**該当**: `crates/ravel-app/src/audio/mixdown.rs:213-219`, `:255-272`,
`crates/ravel-app/src/audio/mod.rs:188-295`

`AudioService::sync` はドキュメント observer から UI スレッド上で、
ライブジェスチャーを含む全編集で走る。
トリムハンドルのドラッグはマウス移動ごとにビルドキーを変えるため、
移動1回あたり最大約 128MiB のサンプル memcpy と、サンプル単位のゲインカーブ評価
（48kHz の数分 = 数百万回のチャンネル評価）が発生する。
リポジトリ自身の「UI スレッド外で行う」ルール違反。

**修正方針**: `build_track` を generation ガード付きでバックグラウンドエグゼキュータへ移す。
またはコミット時のみ再構築し、ジェスチャー中はデバウンスする。

**関連**: [HIGH-15](../closed/HIGH-15-settrack-resamples-on-prep-thread.md)（エンジン側の同種問題）

---

## MED-APP-10 | debt | 解決済み設定のうち autosave / proxy / OCIO に消費側が無い

**該当**: `crates/ravel-project/src/settings.rs`,
`crates/ravel-app/src/app_settings.rs:532-541`

**2026-09-07 に範囲を狭めた。** 起票時は「設定レイヤー全体が一切適用されない」
と書いていたが、その後 `app_settings::install` が `apply(Changed::ALL, cx)` を
呼ぶ形で配線され、**locale / appearance / cache は適用されている**
（`ravel_i18n::set_locale` も `apply_resolved_appearance` も走り、
設定ダイアログからロケールとテーマを選べる）。

残っているのは `apply` が分岐を持たない 3 つ:

- **autosave**（`auto_save_interval_seconds`）— オートセーブタスクが存在しない
- **proxy 再生**（`proxy_resolution`）— 消費側が存在しない
- **OCIO カラー設定** — 消費側が存在しない（`color-management-plan.md` の
  `CM-6` が開くまで動かない）

`app_settings.rs` の外でこれらの名前が出てこないことで確認できる。

**修正方針**: autosave から着手する（ユーザー価値があり、他に依存しない）。
proxy は `VRES-*`、OCIO は `CM-6` に紐づくので、それぞれの計画で消費側が
できるまで dead フィールドのまま置く判断でもよい。

---

## MED-APP-11 | debt | クラッシュ復旧ジャーナルが core に存在するが完全に未配線

**該当**: `crates/ravel-core/src/undo/{journal,recovery}.rs`（`crates/ravel-app` に呼び出し元なし）

ジャーナルの writer / reader と `recover()` のリプレイ機構は実装・テスト済みだが、
アプリは編集時にジャーナルを書かず、起動時に復旧も試みない。
オートセーブ無し（MED-APP-10）+ 保存失敗が不可視
（[CRIT-02](../closed/CRIT-02-save-failure-invisible-and-swallows-quit.md)）と組み合わせると、
クラッシュ時に最後の手動保存以降の作業がすべて失われる。

**修正方針**: `DocumentStore` のコミットにジャーナル書き込みを配線し、起動時に復旧プロンプトを出す。
より安価な暫定策としてオートセーブを先に入れる。

**関連**: [medium/core-evaluator.md](core-evaluator.md) の MED-CORE-08（core 側から見た同じ問題と設計上の障害）

---

## MED-APP-13 | debt | Timeline の行レイアウト走査が4箇所に手動で複製され、チャンネル数のソースが2種類ある

**該当**: `crates/ravel-app/src/panels/timeline.rs:1744`, `:2501`, `:2535`, `:2585-2723`

`keyframes_in_rect`、`row_at_content_y_in`、`total_layer_height`、描画コードが
それぞれ行 / チャンネルの y レイアウトを再導出している。
2つは `row.channel_names.len()`、2つは `row_channels(...)` を使う。
現在一致しているのは ravel-ui `keyframes.rs` の構築の仕方に依存した偶然であり、
乖離すればその行以下すべてでヒットテストと描画が無言でずれる。

**修正方針**: 描画・ヒットテスト・ラバーバンド・高さ計算を駆動する
単一の `(RowHit, y_range)` イテレータを抽出する。

---

## MED-APP-14 | debt | NodeEditorPanel がプロジェクトのレジストリではなく自前の `NodeRegistry` を作る

**該当**: `crates/ravel-app/src/panels/node_editor.rs:474`, `:497-498`

パネルは自分のレジストリに `register_builtins` するが、
authoritative なレジストリは `ProjectState` が所有している（Viewer は `project.registry()` を使う）。
プロジェクトレジストリにのみ登録されたものは Add Node メニュー、`param_range` のクランプ、
カテゴリ色から欠落する。2つが無言で乖離しうる。

**修正方針**: レジストリを `ProjectState` から解決し、プロジェクトが無い場合のみ builtins にフォールバック。

---

## MED-APP-37 | bug | 評価結果が「届いた時点のコンポジション」と対で扱われ、切替中の結果が別コンプの寸法で解釈される

**該当**: `crates/ravel-app/src/project_state.rs:2160-2178`（`ViewerOutput::Frame` /
`ViewerOutput::Gpu` の組み立て）

届いた評価結果に `composition_resolution` を付けるとき、**その結果がどのコンプの
ものかではなく「今アクティブなコンプ」**を読んでいる。コンプ A の評価が飛んでいる
最中に B へ切り替えると、A の絵が **B の解像度**で解釈される。

- 症状 1（従来から）: オーバーレイのコンプ座標変換がずれる。bbox やマニピュレータが
  絵と合わない
- 症状 2（`INSP-3` で増えた）: ピクセル読み取りが**別の画素の値**を報告する
  （`comp_to_buffer_index` がコンプ寸法とバッファ寸法の比を使うため）

`ViewerUpdate` は自分がどのコンプを評価したかを持たないので、**受け取り側では
判定できない**のが根本。`load_project_from` の `load_request` / `revision` ガードと
同じ形（要求時の識別子を結果に添えて、届いたときに突き合わせる）が要る。

→ `ViewerUpdate`（`ravel-core` の `ViewerResult`）に評価したコンプ id を載せ、
アクティブなコンプと一致しない結果は捨てる。捨てるだけで良いのは、切替時には
必ず新しい要求が出ているため。

**検証**: 遅い結果を A のまま作り、B へ切り替えてから配達して、
`ViewerFrame` が更新されない（または A の寸法で解釈される）ことを落とすテスト。

---

## MED-APP-39 | bug | プレビュー解像度を切り替えても、キャッシュ帯が前の係数のまま残ることがある

**該当**: `crates/ravel-app/src/project_state.rs:2229-2252`（`publish_cache_band`）、
`:1767-1774`（`set_viewer_resolution`）

`publish_cache_band` は**フレームキャッシュの version だけ**を見て早期 return する。
帯そのものは `viewer_eval_context`（実効係数を含む）で計算するので、係数が変われば
帯も変わるべきだが、**`set_viewer_resolution` は `published_band_version` を
落とさない**。

新しい係数のフレームがまだキャッシュに無ければ、続く評価で version が上がるので
1 回分の遅れで収まる。**問題は両方がキャッシュに載っている場合** —
`Full` → `1/2` → `Full` と往復すると version が動かないので、帯は
**別の係数で計算したまま**残る。Timeline は「スクラブがタダで済む」と言い、
`INSP-4` の Viewer 右上は同じ帯から割合を出すので、両方が同じだけ嘘をつく。

→ `set_viewer_resolution` で `clear_cache_band`（`published_band_version = None`）
を通す。表示チャンネル・ピクセル読み取りの setter はキャッシュ自体を捨てるので
version が動き、この穴には当たらない。

**検証**: 2 つの係数のフレームを両方キャッシュに入れてから係数を戻し、帯が
戻した係数のものになることを落とすテスト。

---

## MED-APP-38 | bug | 表示設定の切り替えが「飛んでいる評価」を締め出さないので、古い設定のフレームがキャッシュに戻る

**該当**: `crates/ravel-app/src/project_state.rs:1780-1795`（`set_display_channel`）、
`:1840-1855`（`set_pixel_readout`）、`crates/ravel-core/src/runtime/eval_service.rs:836`

どちらの setter も**出力段フレームキャッシュを捨ててから**再要求する
（`INSP-2` / `INSP-3`）。しかし**捨てた時点で走っているワーカーの評価**（先読み
`CACHE-9` を含む）は古い設定で finalize を終え、`clear()` の**後に**キャッシュへ
入りうる。

- チャンネル: 古いモードの表示バイト列が入る → 次のヒットで前のモードの絵が返る
- 読み取り: リニアフレームを持たない（または持ったままの）エントリが入る →
  読み取りが空のまま / off にしたのに f32 を運び続ける

いずれも**次の無効化まで残る**（一過性ではない）。

→ 設定の世代（`u64`）を要求と結果に載せ、**世代が古い結果はキャッシュへ入れない**。
`AudioService` の `generation` と `finish_pending_generation`（#472）が同じ形の前例。

**検証**: 古い世代の結果を配達して、キャッシュに入らないことを落とすテスト。

---

## MED-APP-40 | debt | macOS では GPUI 自身の Metal device の喪失を問う口が fork に無い

**該当**: `crates/ravel-app/src/workspace.rs`（`host_gpu_context` / capability 判定の
`#[cfg(target_os = "macos")]` 腕）、`crates/ravel-app/src/panels/viewer.rs`
（`host_device_loss` を `false` に固定する `cfg(not(...))` 腕）、
gpui fork の `crates/gpui/src/platform.rs`（`gpu_device_lost`）

fork の `PlatformWindow::gpu_device_lost()` と `gpu_context_full()` は
`#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]` の
中にしか無く、`gpui_macos`（Metal ネイティブ）はどちらも実装していない。したがって
macOS では**問う口自体が無い** — `None` が返るのではなく呼べない。

結果として Ravel が macOS で検出できるのは**自前 wgpu device の喪失だけ**である
（`GPULOSS-1` で登録した loss callback）。GPUI の Metal renderer / command queue 側の
喪失と再生成は検出されない。`GPULOSS-4` はこれを安全側で確定させた: 自前 device が
死んだら zero-copy を永久に切って CPU フレームで描き続ける（復旧ではない）。

**実害**: 大きくない。Ravel と GPUI が同じ Metal device を共有している（`ZC-2` の
`native_device_matches` がそれを確認している）ので、GPUI 側の device が死ぬ状況は
Ravel 自前 context の callback も撃つ可能性が高く、そのときは検出できる。「GPUI が自分の renderer を作り直して**別の** device に移った」場合は
**検出できる** — `with_surface_texture` は renderer の現在の device ハンドルを
毎フレーム受け取って `native_device_matches` を通すので、identity が変われば
描かずに `false` を返し、`surface_lost` の経路が capability を落として CPU
フレームを要求する。**残る穴は「identity が変わらないまま device が失われた」
場合だけ** — Ravel 自前 context の callback は撃たれず（別の device なので）、
GPUI 側にそれを問う口が無いので、zero-copy を試み続けて毎 paint が失敗する。
不正なサンプリングは起きない（照合が弾く）ので、症状は「絵が出ない / 出るのが
遅い」に留まる。

**修正方針**: fork の `PlatformWindow` に「callback を上書きせず native device の
identity / loss status を読む口」を macOS でも足せるか調べる（`gpui_macos` の
`MetalRenderer` は `MTLDevice` を保持しているので identity は返せる。喪失は Metal に
`MTLDeviceWasRemoved` 相当の通知があるかに依る）。足せるなら macOS も
`GPULOSS-3` と同じ recovery coordinator に載せられる。足せないなら
`GPULOSS-4` の安全側確定が macOS の最終形になる。

**severity の根拠**: bug ではなく debt。安全側の fallback が既に入っていて、
絵は出続け、不正なサンプリングも起きない。high でないのはデータ損失もクラッシュも
無いため、low でないのは 1 プラットフォームが device 喪失検出を持たない状態が
`GPULOSS-5`（macOS の実機確認）と macOS の recovery 実装をそのまま塞ぐため。

**検証**: fork 側の調査が先。`gpui_macos` に口が付いたら、Linux / Windows と同じ
identity 照合と loss polling のテストを macOS 腕に足す。

---

## MED-APP-41 | debt | zero-copy の可否が session 全体で 1 個なので、別 GPU の 2 枚目の window が main window の zero-copy も落とす

**該当**: `crates/ravel-app/src/project_state.rs`（`configure_viewer_surface` と
`viewer_surface_enabled: Arc<AtomicBool>`）、`crates/ravel-app/src/panels/viewer.rs`
（paint 側の capability 判定）

`viewer_surface_enabled` は `ProjectState` が 1 本だけ持つ共有 atomic で、評価
worker の `DisplayTransform` がそれを読んで「GPU テクスチャを出すか、CPU フレームを
出すか」を決める。**出力の形が session に 1 つしかない**ので、window ごとに
別の答えを持てない。

結果、`done/gpu-device-loss-recovery-plan.md` の `GPULOSS-5` が完了条件に書いた
「device mismatch ならその window だけ CPU fallback」は、実装では
**session 全体が CPU fallback になる**。別 GPU に載った 2 枚目の window を開くと、
main window の zero-copy も一緒に落ちる。

**実害**: 小さい。絵は出続け（CPU 経路）、不正なサンプリングも起きない。マルチ GPU
機で分離 window を使ったときに main window のプレビューが遅くなるだけである。

**修正方針**: worker が 2 つの表現を同時に作るか、capability を window ごとに持って
paint 側が選ぶ。どちらも「zero-copy の可否の権威を 2 つにしない」という
`ZC-8` 以来の方針に触るので、per-window が実際に要る状況（マルチ GPU 機での分離
window）が出てから決める。

**severity の根拠**: bug ではなく debt。安全側の劣化であり、完了条件の記述が
アーキテクチャより広かった。low でないのは、計画書の完了条件と実装が食い違って
いる状態そのものが次の単位の設計を誤らせるため。

## MED-APP-42 | bug | macOS の自前 device 喪失では、退役したフレームが Global に残り続けることがある

**該当**: `crates/ravel-app/src/project_state.rs`（`report_gpu_device_loss`）、
`crates/ravel-app/src/panels/mod.rs`（`ViewerFrame` global）

`GPULOSS-5` は device epoch の交換（`restart_eval_worker`）と session の release で
`ViewerFrame` を blank にした。しかし **macOS の自前 device 喪失の経路
（`report_gpu_device_loss`）は blank しない** — zero-copy を切って CPU フレームを
1 枚要求するだけである。

要求が通れば次のフレームが上書きするので自己解消する。**要求が失敗する経路**
（評価がエラーで返る、worker が既に居ない）では、死んだ device のテクスチャを
運ぶ `GpuFrame` が global に残り続ける。paint 側の guard は identity 照合なので
不正なサンプリングは起きないが、**退役した pool への lease が解放されない**。

**実害**: 中程度。喪失時に一度だけ、そのフレーム分の VRAM が返らない。
`GPULOSS-4` のテストが `ViewerFrame::Frame` の publish を期待しているので、
blank を足すならそのテストの意図（「CPU フレームで描き続ける」）と両立させる形に
する必要がある。

**修正方針**: `report_gpu_device_loss` でも fence 付きで blank し、CPU フレームの
要求が成功したときにそれが上書きされる形にする。`GPULOSS-4` のテストは
「blank → CPU フレーム」の順を見るように書き換える。

**severity の根拠**: bug。ただしクラッシュも不正な絵も起こさず、喪失という
既に劣化した状態でしか踏めないので high ではない。low でないのは、GPU
リソースが返らない経路を残すため。


## MED-APP-43 | bug | macOS では素の Escape が GPUI に一切届かない

**該当**: `gpui_macos/src/window.rs` の `handle_key_event`（ピン rev
`a93d6ccd`、2939-2971 行）

`key_char` を持たないキー（Escape、矢印など）は、修飾キーが無いとまず
ウィンドウの input context に渡される:

```rust
if is_composing || is_ime_printable_key
    || (key_down_event.keystroke.key_char.is_none()
        && !modifiers.control && !modifiers.function && !modifiers.platform)
{
    let handled: BOOL = unsafe {
        let input_context: id = msg_send![this, inputContext];
        msg_send![input_context, handleEvent: native_event]
    };
    if let Some(handled) = ...do_command_handled.take() { return handled as BOOL; }
    else if handled == YES { return YES; }          // ← Escape はここで消える
    let handled = run_callback(PlatformInput::KeyDown(key_down_event));
    return handled;
}
```

`doCommandBySelector:` が呼ばれれば `do_command_by_selector` が KeyDown を
GPUI へ dispatch するので届く。**Escape ではそれが呼ばれず、
`handleEvent:` が YES を返して早期 return する。**

**実測**（2026-09-08、`ravel-widgets` の gallery を release で起動し、
`CGEvent` で送って `observe_keystrokes` にプローブを入れた）:

```
ESCPROBE tooltip entity alive, observer registered
ESCPROBE observer saw Keystroke { key: "tab", key_char: Some("\t") }
ESCPROBE not a dismissal
```

**Tab は届き、Escape は 1 行も出ない。** テキスト入力にフォーカスは無い状態。

**実害**: 中程度。`Window::dispatch_key_event` より下が全部走らないので、
**element のキーリスナ・アクション・`observe_keystrokes` のどれでも
Escape を受け取れない**。`.agents/rules/ux.md` の不変条件 10
（「Escape で離脱」）を満たすことが macOS では原理的に不可能になる。
現に効いていないもの: ツールヒントの Escape 消去、ドラッグの中止
（不変条件 4）。

**テストが通ることに騙されないこと。** `TestAppContext` はプラット
フォーム層を通らないので、`ravel-widgets` の
`escape_takes_down_the_showing` / `escape_dismisses_the_showing_and_nothing_else_does`
は**通るのに実機では動かない**。あの 2 本は「keystroke が配送されたら
正しく畳む」ことの証明であって、配送されることの証明ではない。

**修正方針**: フォーク側で `handleEvent:` の早期 return を Escape に対して
やめる（`do_command_handled` も `handled == YES` も見ずに `run_callback` へ
落とす経路を、`key_char` の無いキーに限って足す）。**上流に投げる価値のある
変更**だが、ユーザーの指示で上流 PR は保留中なのでフォークに載せる。
`gpui_macos` は手元で唯一検証できるプラットフォームなので、
`.agents/rules/` の「検証不能な cfg は判断を外に出す」の対象外。

**severity の根拠**: bug。クラッシュはせず、機能が無いだけ。だが
不変条件 10 と 4 の両方を macOS で満たせなくするので low ではない。

## MED-APP-45 | bug | Viewer の bbox が画像インスタンスの矩形を落とすので、画像ジオメトリのレイヤーが掴めない

**該当**: `crates/ravel-app/src/panels/viewer/geometry.rs` の `geometry_bounds`

`geometry_bounds` は Point / Instance ドメインの**位置だけ**を走査して AABB を作る。

```rust
for domain in [Domain::Point, Domain::Instance] {
    let Some(Ok(positions)) = geometry.positions(domain) else { continue };
    for index in 0..positions.len() { /* min/max だけ */ }
}
```

`geometry.from_image` の出力は「**原点に 1 インスタンス**、画像はインスタンスの
source の `rect()`」という形で、`from_image_outputs_one_instance_stamping_the_image`
（`crates/ravel-nodes/src/geometry.rs`）が 320×180 の画像に対し
`rect = (-160, -90, 320, 180)` を返すことを固定している。位置は 1 点しか無いので
**bbox は 0×0** になる。

結果:

- `layer_comp_rect`（`crates/ravel-app/src/panels/viewer.rs`）が幅 0 高さ 0 を返し、
  `ShellManipulator` の枠とハンドルが実質出ない
- **画像ジオメトリを置いたレイヤーが Viewer から掴めない**。評価はできるが
  編集できない状態（`roadmap.md` の基準 4）
- クリックによるレイヤー選択も AABB 近似なので、同じ理由で当たらない

矩形はインスタンス位置に対して**中心合わせ**（`rasterize/mod.rs` の
`raster_image` が「origin-centred rectangle」と書いている）なので、bbox は
インスタンス位置 ± 矩形の半分を含める必要がある。

`layer-content-size-plan.md` の「問題 2」で見つけた 3 件のうちの 1 つ。
残りは `MED-CORE-11`（コアと Viewer で bbox の定義が違う）と
`LOW-APP-33`（ストローク幅が入らない）。
