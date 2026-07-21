# スモークテスト修正計画

## 背景

実機スモークテストで Viewer / Node Editor / Timeline / 全体 UI にわたる不具合と
UX 不足が確認された。コード調査により主要な不具合の根本原因は特定済み。

- **Viewer の stale 表示**: `ProjectState::on_eval_update`
  (`crates/ravel-app/src/project_state.rs:527-535`) が評価エラー時に early return
  して前回の `ViewerFrame` を保持する。Geometry ノード削除で Rasterize の
  geometry 入力が欠落 (`crates/ravel-nodes/src/rasterize/mod.rs:115-119`) すると
  評価全体が `Err` になり、削除済みジオメトリが描画され続ける。評価器キャッシュ
  自体は `InvalidationHint::Structural` で正しく全消去されている。
- **Bypass でノードが消える**: `bypass_node`
  (`crates/ravel-app/src/panels/node_editor.rs:964-994`) はフラグではなく
  ノード削除として実装されており、1in/1out 以外は再接続なしで削除するだけ。
  トグルとして戻す手段もない。
- **net.in / net.out の増殖・削除**: duplicate / delete / コンテキストメニューに
  境界ノードのガードが存在しない（`ravel_core::network::is_in_node` 等は未使用）。
- **Scatter のズレ**: Rasterize のインスタンススタンプ
  (`crates/ravel-nodes/src/rasterize/mod.rs:529-549`) が `instance_P + ソース生座標`
  で合成するため、原点中心でないソースジオメトリは全コピーがオフセットする。
  anchor / pivot の概念がジオメトリに存在しない。
- **Properties パネルの更新ずれ**: パネルは `ProjectState` を直接 observe せず、
  `SelectedPropertiesTarget` 内のスナップショット (`Box<Layer>`) に依存する。
  Timeline の `publish_selected_layer_target` が再発行しない経路で古い値が残る。
  `PlaybackPosition` observer も Layer ターゲットを明示的にスキップしている
  (`crates/ravel-app/src/panels/properties.rs:558-559`)。
- **Properties パネルがスクロールしない**: ルートに `overflow_y_scroll` は
  付いているが効いていない（要調査の実装バグ）。
- **Playhead が赤**: `timeline.rs:1204` でハードコード `red()`。テーマの
  アクティブ色は `primary` / `ring` = `#5B6EE1`。
- **Keyframe アニメーションが時々止まる**: 再現条件不明。エラー時前フレーム保持
  仕様が原因の可能性が高いが、別原因（キャッシュ frame 判定漏れ等）も否定できない。

## 方針

- バグ修正 (Phase 0) を最優先し、その後 UX 改善を領域別に
  Viewer → Timeline → Node Editor → 全体小物 の順で実施する。
- クレート横断のモデル変更（geometry anchor 属性化、可変入力ポート基盤）は
  独立フェーズとして最後に切り出す。
- 計画書はこの 1 本。PR は論理単位（原則フェーズ内の項目単位）で分割する。
- 各 PR は `mise run check` 通過と `ravel-review` を経ること。

## Phase 0: バグ修正

**ステータス: 完了（2026-07-19）** — 0-1 #92 / 0-2 #93 / 0-3 #96 / 0-4 #94 /
0-5 #95 / 0-6 #97 として全マージ。0-5 は codex 指摘のプロジェクト切替バグを
「同一 LayerId への再ターゲット」ではなく「選択クリア」で解消（別コンプの
同番 LayerId は無関係レイヤーのため）。

### 0-1. Viewer のエラー時 stale 表示

主な対象: `crates/ravel-app/src/project_state.rs`,
`crates/ravel-app/src/panels/mod.rs`, `crates/ravel-app/src/panels/viewer.rs`

作業:

- `ViewerFrame` を `Option<Arc<FrameBuffer>>` から
  Frame / Blank / Error(message) を表現できる状態へ拡張する。
- `on_eval_update` のエラー early return を廃止し、エラー状態を発行する。
- Viewer はエラー時、コンプ解像度の黒フレームの上にエラー内容を小さく
  オーバーレイ表示する。
- Structural 編集直後にエラーが出るケース（Geometry 削除 → Rasterize 入力欠落）を
  再現テストとして固定する。

完了条件:

- Rasterize 上流の Geometry ノードを削除すると、直ちに黒画面 + エラー表示になる。
- Layer / Node 削除後の再評価結果が常に Viewer に反映される。
- エラー解消（再接続等）で通常描画へ復帰する。

### 0-2. Bypass の正規実装

主な対象: `crates/ravel-core/src/graph.rs`, `crates/ravel-core/src/eval.rs`,
`crates/ravel-app/src/panels/node_editor.rs`,
`crates/ravel-app/src/node_editor/painting.rs`

作業:

- `NodeMetadata` に `bypassed: bool` を追加する（永続化含む）。
- 評価器でパススルー処理を実装する: 出力型と一致する最初の入力値を
  そのまま出力する。キャッシュキー妥当性判定にも bypass 状態を反映する。
- 一致する入力を持たないノード（純粋な Generator 等）は Bypass 不可とし、
  コンテキストメニューの Bypass を disable する。
- 現行の削除ベース `bypass_node` を置き換え、トグル（チェック状態表示）にする。
- Bypass 中のノードは canvas 上で半透明表示する。

完了条件:

- 1in/1out 以外のノードでも Bypass が下流を切断しない。
- Bypass のトグルで元の状態へ完全に戻る（undo 含む）。
- Bypass 不可ノードではメニューが無効化されている。

### 0-3. net.in / net.out の保護

主な対象: `crates/ravel-app/src/panels/node_editor.rs`

作業:

- `copy_selected` / `duplicate_selected` / `delete_selected` /
  コンテキストメニューの Delete / Bypass から、
  `ravel_core::network::is_in_node` / `is_out_node` に該当するノードを除外する。

完了条件:

- In / Out ノードの複製・削除・Bypass が UI 上のどの経路でも不可能。
- 各レイヤーネットワークの In / Out がちょうど 1 つずつという不変条件が
  UI 操作で破れない。

### 0-4. Properties パネルのスクロール修正

主な対象: `crates/ravel-app/src/panels/properties.rs`

作業:

- `overflow_y_scroll` が効かない原因を特定して修正する
  （scroll handle の欠落、子要素の min-height 制約等を疑う）。

完了条件:

- パラメータがパネル高さを超えたときスクロールで全項目へ到達できる。

### 0-5. Properties パネルの更新ずれ修正

主な対象: `crates/ravel-app/src/panels/properties.rs`,
`crates/ravel-app/src/panels/timeline.rs`

作業:

- スナップショット依存の観測経路を見直し、Properties パネルが
  `ProjectState` の変更を直接 observe して現在値を再取得する構造にする
  （`SelectedPropertiesTarget` はターゲットの同定のみに使い、値は都度
  ドキュメントから引く方向を基本とする）。
- Layer ターゲットが `PlaybackPosition` 変化で更新されない制限を解消する。

完了条件:

- パラメータ編集 → Timeline へフォーカス移動 → 表示値が最新のまま。
- 再生 / seek 中もアニメーション対象の表示値が追従する。

### 0-6. Keyframe アニメーション停止の調査

主な対象: `crates/ravel-core/src/eval.rs`,
`crates/ravel-core/src/runtime/eval_service.rs`, `crates/ravel-app/src/playback.rs`

作業:

- 0-1 のエラー可視化により、停止時に「評価エラーで止まっている」のか
  「キャッシュが古い値を返している」のかを切り分けられるようにする。
- 評価パイプラインに `tracing` ベースのデバッグログ（frame、hint、キャッシュ
  hit/miss、結果 Ok/Err）を追加し、再現時に原因を特定できる状態にする。
- 再現できた場合はキャッシュ frame 判定・時間依存判定を修正する。

完了条件:

- 停止発生時にログから原因区分を特定できる。
- 判明した原因が修正されているか、未再現の場合はその旨と観測手段が
  ドキュメント化されている。

実施結果（2026-07-19）:

- キャッシュの frame 妥当性・時間依存判定に欠陥は見つからず（コード照査 +
  既存回帰テスト）。0-1 の「エラー時前フレーム保持」が主原因の見込み。
- 第三の候補として **generation 枯渇** を発見（未修正、Phase 1 で対応）:
  評価が 1 フレーム時間より長いと、再生中は完了した結果が常に stale 判定で
  捨てられ、エラーなしで Viewer だけ停止する。`on_eval_update` の採択条件を
  「最後に発行した generation より新しい」へ変えることで修正可能。
- 観測手段: `RAVEL_LOG=ravel_core::runtime::eval_service=debug,ravel_core::eval=debug,ravel_app::playback=debug,ravel_app::project_state=debug`
  （cache hit/miss の詳細は `ravel_core::eval=trace`）。判別:
  worker `ok=false` 連続 = 評価エラー / worker `ok=true` + consumer
  `dropped=true` 連続 = generation 枯渇 / `published=frame` なのに表示が
  変わらず `FrameAdvanced` 無し = 真の stale cache（未観測）。

## Phase 1: Viewer の操作系とツールバー

**ステータス: 完了（2026-07-19）** — generation 枯渇修正 #98（発行を
published_generation ベースの単調採択へ、cancel_pending でフェンス）、
zoom/pan ビューポート基盤 #99（ビューポート数学は
`panels/viewer/viewport.rs`、パンは中ボタンドラッグのみ — Space+左ドラッグは
focus 規約と衝突するため見送り、黒フレームは canvas quad 描画）、
ツールバー #100。

主な対象: `crates/ravel-app/src/panels/viewer.rs`（必要なら
`crates/ravel-app/src/viewer/` へ分割）

### 作業

- **generation 枯渇の修正**（Phase 0 の 0-6 で発見）: `on_eval_update` の
  採択条件を「latest と一致」から「最後に発行した generation より新しい」へ
  変更し、評価が 1 フレーム時間を超えても最新の完了結果が Viewer に届くように
  する。`docs/agent-api-reference.md` の publication contract 記述も更新する。
- 描画を `ObjectFit::ScaleDown` から自前の zoom / offset 変換に置き換える。
  - ホイール = カーソル中心ズーム
  - 中ボタンドラッグ or Space + ドラッグ = パン
- 下部ツールバー（AE 風）を追加する:
  - ズーム倍率の表示 + ドロップダウン（25/50/100/200% 等）
  - Fit ボタン、100% ボタン
  - プロポーショナルグリッド（3x3）トグル
  - セーフエリア（title / action safe）トグル
- コンプが存在する限り、出力が無くてもコンプ解像度の黒フレームを描画する
  （0-1 のエラー黒画面と同一の描画基盤を使う）。グリッド / セーフエリア /
  ズームは空コンプでも機能する。
- ロケール（`assets/locales/`) にツールバー文言を追加する。

### 完了条件

- 1 フレーム時間より重いグラフの再生中も Viewer が更新され続ける
  （consumer の `dropped=true` 連続が発生しない）。
- ズーム / パン / Fit / 100% が動作し、ズームはカーソル位置を維持する。
- グリッドとセーフエリアがコンプ矩形基準で正しくオーバーレイされる。
- 空コンプで黒フレームが表示され、各操作が機能する。

## Phase 2: Timeline の操作系とヘッダ

**ステータス: 完了（2026-07-19）** — 基本操作3点（スクラブ拡張 / playhead
テーマ色 / 空白クリック選択解除）#103、キーフレーム複数選択 #105、
コンテキストメニュー #106（追加要望の Add Layer サブメニューと、
その前提となるレイヤー複製 — `Graph::duplicate_with_fresh_ids` による
全 NodeId 再割当 deep copy — を含む）、キーフレームナビゲータ #107、
ヘッダ拡充 #108。パンは中ボタン相当の既存仕様のまま。Phase 4 の
「アイコンの整備」は #104 として前倒し実施済み（下記）。

主な対象: `crates/ravel-app/src/panels/timeline.rs`,
`crates/ravel-ui/src/panels/timeline.rs`

### 作業

- **スクラブ領域の拡張**: ruler の `on_mouse_down` 後は `TimelineDrag` に
  Scrub 状態を追加して `timeline-root` の `on_mouse_move` / `on_mouse_up` で
  追跡し、ポインタが ruler 外へ出てもスクラブを継続する
  （`widgets/scrub_input.rs` と同じ「mousedown 後はどこでもドラッグ」仕様）。
- **Playhead 色**: ハードコード `red()` をテーマ `primary` に変更する。
- **空白クリックで選択解除**: `layer-area-click` の `RowHit: None` で
  `selected_layer` を解除する。
- **キーフレーム複数選択**: `selected_keyframe: Option<...>` を集合に変更し、
  Shift + クリックのトグル選択、チャンネル行上のラバーバンド選択、
  複数キーフレームの一括移動 / 削除に対応する。
- **コンテキストメニュー**（gpui-component `ContextMenuExt`、Node Editor と
  同パターン）:
  - レイヤーバー上: 削除 / 複製 / Solo / Mute / Lock
  - チャンネル行上: この位置にキーフレーム追加
  - キーフレーム上: 削除
- **キーフレームナビゲータ**: プロパティ行左に ◀ ◆ ▶ を配置し、
  前後キーフレームへのジャンプと現在フレームのキーフレームトグルを行う。
- **ヘッダ拡充**:
  - タイムコード表示（HH:MM:SS:FF + フレーム番号、クリックで直接入力ジャンプ）
  - fps / デュレーション表示
  - トランスポートボタン（再生 / 停止、コマ送り、先頭 / 末尾）
  - ppf ズームスライダーと全体フィットボタン
- ロケール文言を追加する。

### 完了条件

- スクラブ開始後、ポインタ位置によらず playhead が追従する。
- 複数キーフレームの選択・移動・削除が動作し、単一選択の既存挙動が保たれる。
- コンテキストメニューとナビゲータの各項目が対応する編集を 1 回だけ実行する。
- タイムコード入力で正確にジャンプする。

## Phase 3: Node Editor の操作系と視覚言語

**ステータス: 完了（2026-07-19）** — クリック位置へのノード追加 #111、
z-order #112（`NodeMetadata.z` 追加 + journal format v4、掴んだ時点で
最前面へ raise。移動を伴わないクリックの raise は表示のみで、次の移動
コミットで永続化）、ポート形状 #113、カテゴリ色 #114（上端 3px アクセント
バー）、エッジドロップ Add Node メニュー #115（計画追記は #110。
gpui-component の `ContextMenu` はプログラム的に開けないため、
`PopupMenu::build` + `deferred(anchored())` + `DismissEvent` 購読を
パネル内でミラーする実装）。

実機フィードバックによる #114 の再実装（2026-07-20）: ①バー (3px) が
角丸半径 (6px) より低く角丸カーブが本体輪郭からはみ出す → ヘッダ領域
全体（高さ > 角丸半径）への低アルファ背景に変更。②機能軸カテゴリ
（Generator / Filter / Transform …）がデータドメインを横断し、
geometry.transform が Image 系 transform と同色になる → `NodeCategory`
をドメイン軸（Geometry / Field / Image / Color / Time / Utility）に
再編し、カテゴリ色 = 対応するデータ型の `port_color` と 1:1 に。
Add Node メニューの分類・ロケールも追従。

主な対象: `crates/ravel-app/src/panels/node_editor.rs`,
`crates/ravel-app/src/node_editor/painting.rs`,
`crates/ravel-app/src/node_editor/port_colors.rs`,
`crates/ravel-core/src/graph.rs`

### 作業

- **クリック位置へのノード追加**: `add_node_from_template` のハードコード
  (200, 200) を廃止し、`last_right_click` を `screen_to_flow` 変換した座標へ
  配置する。
- **z-order**: `NodeMetadata` に z 値（単調増分カウンタ）を追加し、
  ドラッグ開始時に最前面へ引き上げ、`paint_nodes` を z 順で描画する。
- **ポート形状の型分け**（色は現行の `port_color` を維持）:
  - Geometry = ダイヤ、FrameBuffer = 角丸四角、Field = 三角、その他 = 円
  - エッジのドラッグ判定・接続 UI も形状に追従させる。
- **カテゴリ色**: `NodeCategory` → 色のマップを追加し、ノードヘッダに
  カテゴリ別のアクセント（薄いヘッダ背景）を描画する。カテゴリは
  データドメイン軸（Geometry / Field / Image / Color / Time / Utility）
  とし、色は対応するデータ型の `port_color` をそのまま使う（ヘッダと
  ポートドットが同じパレットで揃う）。
- **エッジドロップでのノード追加**（Houdini / Nuke 風）: ポートからの
  エッジドラッグを何もない場所（ポート・ノード・エッジ以外）で離したとき、
  リリース位置に Add Node メニュー（右クリックメニューと同じカテゴリ構成）を
  開く。ノードを選択するとリリース位置に配置し、ドラッグ元ポートと型互換の
  最初のポートへ自動接続する — 出力からのドラッグは新ノードの最初の互換入力へ、
  入力からのドラッグは新ノードの最初の互換出力へ。入力側の既存エッジは通常の
  接続操作と同じく置換する。型互換ポートを持たないノードを選んだ場合は
  接続なしで配置のみ行う。メニューをキャンセルした場合はグラフを変更しない。

### 完了条件

- 右クリック位置に新規ノードが出現する。
- 掴んだノードが最前面に描画され、その順序が永続化される。
- ポート形状 / カテゴリ色が全ビルトインノードで一貫している。
- エッジを空白で離すと Add Node メニューが開き、選択したノードが
  リリース位置に接続済みで出現する（配置 + 接続で 1 undo ステップ）。
  キャンセルでグラフが変化しない。

## Phase 4: 全体の小物と磨き込み

**ステータス: 完了（2026-07-21）** — 「アイコンの整備」は #104 で前倒し完了
（◆/◇ → diamond / diamond-filled、▼/▶ → chevron、○/◎/● →
circle / circle-dot / circle-filled、トグル類にツールチップ追加、
Timeline のキーフレーム描画も回転ダイヤの `paint_path` に変更）。
チェックマーク類は gpui-component 部品内蔵アイコンのみで生グリフ
使用箇所なし。アクティブ色の統一 #118（キーフレーム ◆ / ナビゲータ /
Solo・Mute・Lock / ポートトグル ◎● を `accent` → `primary`。レイヤー
バーと scrub ドラッグ背景は操作状態・中立色のため対象外）、Titlebar
プロジェクト名 #119（中央に muted で表示、OS ウィンドウタイトルは
`<project> — Ravel` に `observe_in` で同期 — タイトルが実際に変わる
ときだけ再セット）。**フォーカスパネルの枠は実装後にユーザー判断で
見送り**: `border_1` は GPUI レイアウト上コンテンツ領域を削るため
フォーカス移動でパネル内容が 1px シフトし、ドックのスプリッタに
左辺が隠れる問題もあった。パネル判別はタブタイトルの既存フォーカス
色分けで足りる。

主な対象: `crates/ravel-app/src/title_bar.rs`,
`crates/ravel-app/src/panels/*.rs`, `crates/ravel-app/src/assets.rs`,
`assets/icons/`, `assets/themes/ravel.json`

### 作業

- **アクティブ色の統一**: キーフレーム済み ◆、Solo / Mute / Lock の
  アクティブ状態、Timeline のダイヤ等で `colors.accent`（muted な灰）を
  使っている箇所をテーマ `primary` 系へ変更し、アクティブ状態を視認可能にする。
- **Titlebar にプロジェクト名**: `project_path` の file stem
  （未保存時は Untitled 相当のロケール文言）を `"<project> — Ravel"` 形式で
  表示し、OS ウィンドウタイトルも同期する。
- **アイコンの整備**: Unicode グリフを lucide SVG へ置換する
  （◆/◇ → diamond、▼/▶ → chevron、チェックマーク → check。
  ポートトグル ○/◎/● は視認性を確認のうえ適切なアイコンを選定）。
  `RavelIcon` に必要なバリアントを追加する。
- **フォーカスパネルの枠**: 既存の `FocusedPanelGlobal` を読み、フォーカス中
  パネルのボディ枠を `ring` 色で描画する。`.agents/rules/gpui.md` に従い
  render 中の focus 変更は行わない（読み取りのみ）。

### 完了条件

- キーフレーム / Solo 等のアクティブ状態が一目で判別できる。
- プロジェクトの開閉・保存で Titlebar 表示が追従する。
- 置換対象のグリフがすべて SVG アイコンになり、ライト / ダーク両テーマで
  視認できる。
- フォーカス移動でパネル枠のハイライトが正しく移る。

## Phase 5: geometry anchor 属性化と Scatter のズレ解消

**ステータス: 完了（2026-07-21）** — 仕様確定 #122、anchor 属性・generator
設定・transform 追従 #123、scatter の `center_input` 中央寄せスタンプ
（テンプレート既定 ON / プロセッサフォールバック OFF の二層既定で既存
プロジェクトの描画互換を維持）#124、新規 shape ノードのコンプ中心配置
（app 層のノード追加時上書き）#125。Rasterize・merge・直列化は無変更、
`JOURNAL_FORMAT_VERSION` bump 不要。

クレート横断のモデル変更。仕様は「確定仕様」節のとおり確定済み。

主な対象: `crates/ravel-core/src/geometry/`,
`crates/ravel-nodes/src/geometry.rs`, `crates/ravel-nodes/src/scatter/mod.rs`,
`crates/ravel-nodes/src/rasterize/mod.rs`, `crates/ravel-core/src/registry/builtin.rs`

### 作業

- Geometry コンテナに detail 属性 `anchor: Vec2` を追加する。
- Generator（ellipse 等）は自身の形状中心を anchor に設定する。
  あわせて Generator の既定生成位置をコンプ解像度の中心に変更する
  （新規ノードのみ。既存ドキュメントの値は変更しない）。
- `geometry.transform` は点 `P` と同じ変換を anchor にも適用する。
- Scatter は「各点 − anchor」でソースをスタンプする。`center_input`
  チェックボックス（default ON）を追加し、OFF で従来の生座標スタンプに戻せる。
  anchor 未設定のジオメトリは AABB 中心（`bounds_center`）へフォールバックする。
- Rasterize の通常描画（インスタンス以外）は anchor を無視し、
  既存の描画位置を変えない。
- 直列化互換: anchor 属性が無い既存プロジェクトを読み込めること。

### 確定仕様

- **anchor の表現**: `Geometry` の detail 属性 `anchor`
  （`AttributeArray::Vec2`、要素数 1）。名前定数を
  `crates/ravel-core/src/geometry/names.rs` に `ANCHOR` として追加する。
  `Geometry` は直列化されないランタイム評価産物のため、プロジェクト形式・
  undo ジャーナルの変更はなく `JOURNAL_FORMAT_VERSION` の bump も不要。
- **Generator の anchor 設定**: `shape.rect` / `shape.ellipse` /
  `shape.polygon` / `shape.star` が `(center_x, center_y)` を anchor に
  設定する。`shape.custom_path`（空ジオメトリのプレースホルダ）は設定しない。
- **既定生成位置**: registry テンプレートの既定値は `(0, 0)` のまま変更しない。
  shape 系ノードの作成時に `center_x` / `center_y` を所属コンポジション解像度の
  中心で上書きする。対象経路はノードエディタの追加（Add Node / エッジドロップ、
  app 層）と、レイヤーテンプレートの実体化（`add_layer_from_template`、
  ravel-ui — Layer メニューの Shape 等）の両方で、ヘルパ
  `apply_shape_center_default` を共有する（新規ノードのみ。core・
  既存ドキュメント・既存テスト / ゴールデンに影響しない）。
- **transform の anchor 適用**: point `P` と同一の変換
  （pivot 周りの scale → rotate → translate）を anchor に適用する。
  identity 短絡（入力 `Arc` のパススルー）は維持する。`use_centroid` の
  pivot 計算は従来どおり AABB 中心のままとし、anchor を pivot に使わない
  （非対称形状で既存描画が変わるため）。
- **bounds_center フォールバックの定義**: point `P` の AABB 中心 →
  点が無ければ instance `P` の AABB 中心 → どちらも無ければ anchor なし
  （従来の生座標スタンプ）。既存
  `crates/ravel-nodes/src/geometry.rs` の `bounds_center` と同一定義を
  core の geometry ops へ昇格して共有する。
- **Scatter のスタンプ方式**: `center_input` ON のとき、instance `P` から
  anchor を減算するのではなく、anchor 分だけ平行移動したソース複製
  （point / instance の `P` 列から anchor を減算し、複製側の anchor detail を
  `(0, 0)` に更新したもの）を `instance_source` に設定する。instance `P` は
  従来どおり配置座標を保持するため、回転・スケール付き instance
  （circular の align_rotation 等）でも回転中心がずれない。Rasterize は
  GPU / CPU とも無変更。
- **center_input の対象と既定値**: scatter 系 4 ノード
  （`scatter.grid` / `scatter.circular` / `scatter.path_array` /
  `scatter.scatter`）すべてに `Bool` パラメータとして追加する。
  registry テンプレート既定は ON（新規ノードは param を明示的に保持）、
  プロセッサのフォールバック既定は OFF
  （`bool_or("center_input", false)`）。param を持たない既存プロジェクトの
  ノードは従来挙動のままとなり、描画互換を保つ。bool パラメータは
  properties パネルで自動的にチェックボックス描画される。ラベルは
  `[properties.field]` に en / ja 両ロケールで追加する。
- **geometry.merge の anchor**: detail 属性の A 側優先マージを維持する。
  merge 出力の anchor は第 1 入力のものになる（本フェーズでは変更しない）。
- **Scatter 出力自身の anchor**: 設定しない。scatter を別の scatter の
  ソースにする多段構成はフォールバック（instance `P` の AABB 中心）で
  中央寄せされる。

### 完了条件

- Ellipse を移動して Scatter に接続しても、コピーがズレなく instance 位置に
  スタンプされる。
- `center_input` OFF で従来挙動を再現できる。
- 既存プロジェクトの読み込みと通常描画結果が変化しない（ゴールデン比較 or
  既存テストで担保）。

## Phase 6: 可変入力ポート基盤と Scatter 複数ソース

**ステータス: 完了（2026-07-21）** — 仕様確定 #131、Geometry 複数
instance_sources + `source_index` 属性と rasterize の per-instance 選択 #132、
可変入力ポート基盤（`InputPort.is_variadic` +
**`JOURNAL_FORMAT_VERSION` 5 へ bump**、grow / compact + reindex、
テンプレート宣言、ロード時 migration）#133、scatter 4 ノードの可変
グループ化 + `source_mode` / `source_seed` + エディタ接続・切断経路の
grow / compact フック #134。実装中の追加確定: 可変グループは
「固定入力の直後・露出パラメータポートの前」の連続領域で、grow は
param ポートの前へ挿入しそれらのエッジを reindex する（#134 で
migration も同順序へ正規化）。これで本計画の全フェーズが完了。

クレート横断のモデル変更。仕様は「確定仕様」節のとおり確定済み。

主な対象: `crates/ravel-core/src/graph.rs`,
`crates/ravel-core/src/registry/mod.rs`, `crates/ravel-core/src/eval.rs`,
`crates/ravel-app/src/panels/node_editor.rs`,
`crates/ravel-app/src/node_editor/painting.rs`,
`crates/ravel-nodes/src/scatter/mod.rs`

### 作業

- ノードテンプレートに「可変入力グループ」を宣言できる基盤を追加する
  （Houdini Merge 風: 接続すると次の空ポートが生える。切断で詰める）。
- グラフモデル・直列化・undo・評価器の入力解決を可変ポートに対応させる。
- Node Editor の描画とヒットテストを動的ポート数に対応させる。
- Scatter の `instance_source` を可変入力グループ化し、
  `source_mode` パラメータ（sequential / random + seed）で instance ごとの
  ソース選択を行う。
- 既存の `geometry.merge` 等、可変入力が自然なノードへの展開は本フェーズでは
  行わない（基盤の汎用性のみ担保する）。

### 確定仕様

- **可変入力グループの表現**: `InputPort` に `#[serde(default)]` の
  可変グループ所属フラグを追加する（`is_param` と同じ additive 規約、
  `skip_serializing_if` 禁止）。**`InputPort` の直列化レイアウトが変わるため
  `JOURNAL_FORMAT_VERSION` を 5 に bump** し、`undo/journal.rs` の版数
  コメントに追記する。`NodeTemplate` は可変グループを「inputs 末尾の連続
  領域」として宣言し、`create_node` は空きスロット 1 個で開始する。
- **grow / compact の意味論**: グループ内の全スロットが接続済みになったら
  空きスロットを 1 個末尾に append する（`expose_param_port` と同じく
  末尾追加で既存エッジ index は安定）。グループ内スロットへのエッジが
  切断されたらそのスロットを削除し、後続スロットを狙うエッジの
  `target_port` をデクリメントする（`remove_param_port` の
  compact + reindex を可変グループへ一般化）。グループは常に末尾に
  空きスロットを 1 個だけ持つ。フックはノードエディタの接続確定・切断・
  再接続経路で、`replace_node` を含む新グラフを `commit_graph` に渡す。
- **undo**: エディタ undo は Document スナップショット単位のため、
  ポート増減はエッジ変更と同一コミットに同梱され自動で undo / redo される。
  `GraphMutation` への新 variant 追加は不要（クラッシュ回復ジャーナルは
  `AddNode(Node)` がポート状態ごと運ぶ）。
- **直列化と互換**: プロジェクト RON はノードを verbatim round-trip し
  テンプレート再導出しないため、可変スロットはそのまま保存・復元される。
  旧プロジェクトは `normalize_net_in_ports` と同系のロード時 migration で
  scatter 系ノードの `instance_source` ポートへ可変フラグを付与し、
  接続済みなら空きスロットを末尾 append する（末尾追加なのでエッジ index
  安全）。
- **ポート命名**: グループ先頭は既存名 `instance_source` を維持し、
  以降 `instance_source_2`, `_3`, … の連番。compact 時に連番を振り直す
  （エッジは index 参照のため名前変更は表示のみに影響）。
- **複数ソースの保持**: `Geometry` の `instance_source: Option<Arc<Geometry>>`
  を複数形 `instance_sources`（`Vec<Arc<Geometry>>`）へ拡張し、既存の
  単一 API は先頭要素の互換ラッパとして残す。instance 属性
  `source_index`（I32、`names` に定数追加）で各 instance の参照先を示す。
  Rasterize の instance 展開（GPU flatten / CPU リファレンス両方）は
  `source_index` でソースを選択し、属性が無い場合は従来どおり先頭ソース
  （既存ゴールデン・既存テスト無傷）。Geometry は非直列化のため形式影響なし。
- **source_mode / source_seed**: scatter 系 4 ノード
  （`scatter.grid` / `scatter.circular` / `scatter.path_array` /
  `scatter.scatter`）すべてに enum パラメータ `source_mode`
  （`sequential` / `random`、`with_param_options` 基盤）と Int パラメータ
  `source_seed` を追加する。sequential は instance `i` → source
  `i mod N`、random は `source_seed` による決定的選択。`scatter.scatter` の
  既存 `seed`（配置用）とは独立。接続ソースが 1 個以下のときは
  `source_mode` に関わらず従来動作。
- **可変グループ化の対象**: scatter 系 4 ノードの `instance_source` を
  一律可変グループ化する（`scatter.path_array` はグループが index 1 以降 —
  基盤は「末尾連続領域」を前提とし、先頭固定ポートと共存できること）。
- **center_input との合成**: Phase 5 の中央寄せ（−anchor 平行移動複製）は
  各ソース複製へ個別に適用する。
- **評価器 / UI**: 入力解決は既存の per-index スロット
  （`node.inputs.len()` ぶんの `Option`）で無変更。ポート描画・
  ヒットテスト・ノード高さは live `Node.inputs` 駆動のため動的数に自動追従
  し、描画側の変更は原則不要（実装中に例外が出れば本節に追記）。
- **ロケール**: `source_mode` / `source_seed` のラベルと enum 選択肢を
  en / ja 両方に追加する。

### 完了条件

- Scatter に複数ジオメトリを接続でき、sequential / random で振り分けられる。
- random は seed 固定で決定的に再現する。
- ポートの増減が undo / redo / 保存・再読込で破綻しない。
- 旧プロジェクト（可変フラグなしの scatter ノード）が読み込み後に
  可変グループとして機能し、既存エッジと描画結果が保たれる。
- 単一ソース接続時の描画結果が従来と一致する（既存テスト / ゴールデンで
  担保）。

## 検証

- 各 PR で `mise run check`（fmt、pattern lint、clippy、workspace tests）を通す。
- PR 前に `ravel-review` を実行する。
- Phase 0 の各項目は再現手順を PR 説明に残し、修正前後の挙動を比較できるように
  する。
- Phase 5 / 6 は既存プロジェクトの読み込み互換を必須確認項目とする。
