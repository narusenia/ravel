# ポインタフィードバック 実装計画

> **Status**: Complete — PR #TBD, 2026-07-30

対象: Timeline / Viewer / NodeEditor の canvas 上で、ポインタの下にあるものと
進行中のジェスチャーをマウスカーソルの形で示す。
関連要件: REQ-UI-003（Timeline）、REQ-UI-011（Viewer ツール）、REQ-UI-012
（カーブエディタ）、REQ-UI-002（ノードグラフ）。

## 問題

**掴めるものと掴めないものが見た目で区別できない。** 3 パネルとも主要な操作面が
1 枚の `canvas` 描画で、そこに要素が無いためカーソルは常に `Arrow` のままになる。

現状の分布を全パネル走査した結果:

| 層 | 状態 |
|---|---|
| ボタン・行・トグルなどの要素 | ほぼ設定済み（`timeline.rs:2234` 他、`outliner.rs:729`、`media_bin.rs:311`、`properties.rs:374`） |
| 数値スクラブ | `widgets/scrub_input.rs:313` が `ResizeLeftRight`。ドラッグ中も維持される |
| ドックのタブ・分割ハンドル・テキスト入力 | gpui-component 側で設定済み |
| **canvas 上の当たり判定（バー / トリムエッジ / キーフレーム / ポート / ノード / エッジ / パスハンドル）** | **すべて `Arrow`** |
| **進行中のジェスチャー** | **すべて `Arrow`** |

痛みが最も大きいのは、**当たり判定が狭くて目に見えない箇所**:

- Timeline のトリムエッジは左右 6px（`TRIM_HANDLE_PX`、`timeline.rs:81`）。
  1px 外すとバー全体の移動になる
- ロックされたレイヤーはバーを押しても何も起きない（`timeline.rs:4056`）。
  理由が画面に出ない
- NodeEditor のポートはノード本体と隣接し、どこからが接続開始か分からない
- Viewer で描画ツールを選んでも見た目が変わらないので、いま描画モードなのか
  選択モードなのか押してみるまで分からない

## 使える機構と使えない機構

`gpui-ce` の rev `c1faa21` で確認した内容。

| やりたいこと | 手段 | 出典 |
|---|---|---|
| 要素にカーソルを付ける | `Styled::cursor(CursorStyle)` / `cursor_pointer()` などの生成メソッド | `gpui_macros/src/styles.rs:159-330` |
| 追加の `id` やヒットボックス宣言 | **不要**。`mouse_cursor` が設定されているだけでヒットボックスが挿入される | `gpui/src/elements/div.rs:2070-2073` |
| `on_drag` 中の維持 | 自動。ドラッグ元要素の `mouse_cursor` がそのままドラッグ中のカーソルになる | `div.rs:2527`, `:2621`（`scrub_input.rs` が実例） |
| 自前のドラッグ状態での維持 | paint フェーズで `Window::set_window_cursor_style` を呼ぶ。要素側より優先される | `gpui/src/window.rs:3138-3142` |
| 使えるカーソル | `CursorStyle` の 24 種（`Arrow` / `IBeam` / `Crosshair` / `OpenHand` / `ClosedHand` / `PointingHand` / 各 `Resize*` / `OperationNotAllowed` / `DragCopy` / `DragLink` / `ContextualMenu` 他） | `gpui/src/platform.rs:1794-1877` |
| **独自ビットマップカーソル** | **不可**。プラットフォーム trait が `set_cursor_style(CursorStyle)` の固定 enum しか受けない | `gpui/src/platform.rs:214` |
| **任意タイミングでの OS カーソル非表示** | **不可**。`CursorHideMode` はキー入力に連動した自動非表示のポリシーだけ | `gpui/src/app.rs:280-290, 919-926` |

したがって**本計画は既定の 24 種の割り当てに限る**。独自カーソル画像や
カーソル自体の自前描画は `gpui-ce` へのパッチが前提になるので扱わない。

もう 1 つの制約: **gpui-component の `Button` は `cursor_default()` を明示設定**して
おり、link / text variant だけが `PointingHand` になる（`button.rs:482-491`）。
ボタン類のカーソルを変えるには ravel 側のラッパーか fork へのパッチが必要なので、
**本計画はボタンに触らない**。

## 決定事項

### hover 判定は既存のヒットテストを使う。新しい走査を増やさない

3 パネルとも mousedown 用のヒットテストが揃っており、hover 判定はその再利用で足りる。

| パネル | 既存ヒットテスト |
|---|---|
| Timeline | `bar_hit` :1010（`BarZone::{Body, InEdge, OutEdge}`）、`row_at_content_y`、`keyframe_at_content_x` :1731、`graph_hit_at` :4150 |
| NodeEditor | `node_at_local_pos` :1365、`port_at_local_pos` :1371、`painting::edge_at_local_pos` :714 |
| Viewer | `hit_test_shape_nodes` :1942、`path_handle_hit` :2072、`comp_hit_radius` :975、`pen_should_close` :2065 |

`MED-APP-13`（Timeline の行レイアウト走査が 4 箇所に散っている）を悪化させないため、
**hover 判定は既存関数の呼び出しに限り、レイアウト計算を新たに書かない**。

### カーソルの決定はヒント → カーソルの純粋写像に閉じる

パネルごとに hover 状態を表す enum（`PointerHint`）を持ち、
「ヒント → `CursorStyle`」を引数だけで決まる関数にする。テストはこの関数と
ヒット判定の組み合わせに対して書く。GPUI の描画結果は検証対象にしない。

```text
on_mouse_move ─→ 既存ヒットテスト ─→ PointerHint
                                        │
                          変化した？ ──no─→ 何もしない（notify しない）
                                        │yes
                                    フィールド更新 + cx.notify()
                                        │
render ─→ cursor_for(hint, drag_state) ─→ .cursor(style)
                                        │
paint（ドラッグ中のみ）─→ set_window_cursor_style(style)
```

### hover のたびに再描画しない

Timeline のレーンは行仮想化が無く（`MED-UI-03`）、Viewer と NodeEditor も
canvas 全面を描き直す。**ヒントが変化しない mouse move で `cx.notify()` を
呼ばない**ことを各単位の完了条件に置く。ヒントが変わるのは境界を跨ぐ瞬間だけなので、
再描画の頻度は「バーの端に入った / 出た」程度に収まる。

### ドラッグ中は要素の hover から切り離す

自前のドラッグ状態（`TimelineDrag` / `DragMode` / `pan_drag` など）はポインタが
要素の外へ出ても続く。この間は paint フェーズで `set_window_cursor_style` を使い、
ポインタ位置に関係なくジェスチャーのカーソルを保つ。`on_drag` 経路
（`scrub_input.rs`）は既に自動でそうなっているので触らない。

### 機能が無いものにカーソルを付けない

カーソルは「ここで何ができるか」の約束になる。**実装が無い操作に対しては
何も変えない**。該当は 2 件で、どちらも既存 issue が別に扱う。

- Hand / Zoom ツール（`MED-APP-15`）: ツールバーと `h` キー（`viewer.rs:1815`）で
  状態は変わるが、`select_mouse_down` :398 が Select 以外を弾くため何も起きない。
  `OpenHand` を出すと「掴めるのに動かない」状態になる
- Viewer の選択 bbox の 8 ハンドル（`selection_handle_centers` :2720）: 描画のみで
  リサイズ操作が無い。`Resize*` を付けると同じ問題になる

どちらも**ロードマップのフェーズ E**（`OVL-*` と「操作の正しさ」クラスタ）で
機能側と一緒に扱い、そのときカーソルも同じ単位で付ける。

## 実装単位

### PTR-1: 判定不要の静的カーソル

hover 判定を必要としない箇所だけを先に入れる。機構は導入しない。

- Viewer: 描画ツール選択中（`ToolKind::{Pen, Rect, Ellipse}`）の canvas 面を
  `Crosshair`。`ToolState` はグローバルなのでポインタ位置を見ない
- Timeline: ルーラー（`ruler-scrub` div、`timeline.rs:3620`）を
  `ResizeLeftRight`
- NodeEditor: canvas 面の既定を `Crosshair`（空白ドラッグが矩形選択なので）
- Timeline: カーブエディタのグリッド面（`build_curve_editor_shell`）の既定を
  `Crosshair`

**完了条件**

- ツールを切り替えるとカーソル指定が変わることを、ヒント写像のユニットテストで示す
- 静的カーソルの追加で `cx.notify()` の呼び出し箇所が増えていないこと
- 実機で 4 箇所を目視確認（変更点と確認手順を PR 本文に書く）

### PTR-2: ヒント機構とドラッグ中の保持（Timeline で導入）

- `PointerHint` と `cursor_for()` を Timeline パネルに置く。定義は
  `crates/ravel-app/src/panels/` 配下（`CursorStyle` に依存するので
  コア層・`ravel-ui` には置かない）
- `on_mouse_move`（`timeline.rs:3519`）にドラッグ中でないときの hover 判定を追加し、
  **ヒント変化時のみ** `cx.notify()`
- `TimelineDrag` の各 variant に対応するカーソルを paint フェーズの
  `set_window_cursor_style` で保持する

**完了条件**

- 同じ位置での連続 mouse move が 1 回しか notify しないテスト
- ドラッグ中は hover 判定を行わない（ヒントを更新しない）テスト
- `render()` に状態変更が入らないこと（`.agents/rules/gpui.md` の render 純粋性）

### PTR-3: Timeline の割り当て

`PTR-2` の機構に Timeline のヒットテストを繋ぐ。

| 対象 | hover | ドラッグ中 |
|---|---|---|
| バー本体（`BarZone::Body`） | `OpenHand` | `ClosedHand`（`MoveBar`） |
| トリムエッジ（`InEdge` / `OutEdge`） | `ResizeLeftRight` | `ResizeLeftRight`（`TrimIn` / `TrimOut`） |
| ロックされたレイヤーのバー | `OperationNotAllowed` | — |
| キーフレーム菱形 | `PointingHand` | `ClosedHand`（`MoveKeyframe`） |
| レイヤー行の空白 | `Crosshair` | `Crosshair`（`RubberBand`） |
| レイヤーヘッダー行 | `Arrow`（変更なし） | `ResizeUpDown`（`Reorder`） |
| ルーラー | `PTR-1` で設定済み | `ResizeLeftRight`（`Scrub`） |
| グラフのアンカー | `PointingHand` | `ClosedHand`（`GraphKeyframes`） |
| グラフのタンジェント | `Crosshair` | `Crosshair`（`GraphTangent`） |
| グラフの空白 | `Crosshair` | `Crosshair`（`GraphRubberBand`） |

**完了条件**

- `bar_hit` の 3 ゾーンがそれぞれ期待するカーソルに写るテスト
- ロック済みレイヤーが `OperationNotAllowed` になるテスト（同じ判定を
  mousedown 側が使っていることの確認を含む）
- Graph モードと Dopesheet モードでヒントが混ざらないテスト
- トリムエッジ 6px の内外でヒントが切り替わることを実機で目視確認

### PTR-4: NodeEditor の割り当て

| 対象 | hover | ドラッグ中 |
|---|---|---|
| ポート | `Crosshair` | `Crosshair`（`Connect`）、スナップ確定時は `DragLink` |
| ノード本体 | `OpenHand` | `ClosedHand`（`MoveNodes`） |
| エッジ | `PointingHand` | — |
| 空白 | `Crosshair`（`PTR-1`） | `Crosshair`（`SelectBox`）、`ClosedHand`（`Pan`） |

`on_mouse_move`（`node_editor.rs:1978`）は既に常時発火するので分岐追加で足りる。
ポート → ノード → エッジの優先順は mousedown 側の順序に一致させる
（`HIGH-22` で z 順を直した経路をそのまま使い、hover だけ別の順序にしない）。

**完了条件**

- ポートとノード本体が重なる位置で hover が mousedown と同じ対象を選ぶテスト
- スナップ対象がある `Connect` 中のカーソルが変わるテスト
- ヒント不変の mouse move が notify しないテスト

### PTR-5: Viewer の割り当て

| 対象 | hover | ドラッグ中 |
|---|---|---|
| 描画ツール選択中 | `Crosshair`（`PTR-1`） | `Crosshair` |
| 中ボタンパン | — | `ClosedHand`（`pan_drag`） |
| 選択レイヤー / シェイプの本体 | `OpenHand` | `ClosedHand`（`move_drag`） |
| パスのアンカー | `PointingHand` | `ClosedHand`（`path_edit_drag`） |
| パスのタンジェント | `Crosshair` | `Crosshair` |
| ペンでパスを閉じられる位置 | `DragCopy` | — |

`hit_test_shape_nodes` と `path_handle_hit` はコンプ空間で判定するので、
`comp_position` が `None`（フレーム外）のときはヒントを更新しない。

実装時点の gpui-ce `CursorStyle` に汎用 `Move` variant は無いため、本体 hover は
同じ「掴んで移動」を表す `OpenHand` を使用する。NodeEditor のノード本体とも
割り当てが揃う。

**完了条件**

- 選択レイヤーの内外でヒントが変わるテスト（未選択レイヤー上では変わらない）
- パスのアンカーとタンジェントが別のヒントになるテスト
- `pen_should_close` が真の位置だけヒントが変わるテスト
- 殻の変換が非恒等なレイヤーでヒント境界が図形に一致することを実機で目視確認

### PTR-6: Outliner の並べ替えと文書

- Outliner のレイヤー行ドラッグ（`start_layer_drag` :755 / `drag_over_row` :742）中を
  `ResizeUpDown`。行の hover は既存の `PointingHand` のまま
- `docs/specifications/ui-spec.md` に「ポインタフィードバック」節を追加し、
  パネルごとの割り当て表を置く（本計画の表を仕様側の正にする）
- `docs/gpui-ui-guide.md` に、canvas 上のパネルへカーソルを追加する手順
  （既存ヒットテスト再利用・notify ゲート・ドラッグ中の window cursor）を記載
- `docs/ui-impl-status.md` の該当パネル表を更新

**完了条件**

- 3 文書が実装と一致し、未実装（Hand / Zoom、bbox リサイズ）を実装済みとして
  書いていないこと
- ロケール追加は無い（カーソルに文字列は無い）ことを確認

## 検証

- `mise run check`
- ヒント写像とヒットテストの組み合わせは `ravel-app` のユニットテストで覆う。
  GPUI 統合テストは追加しない（カーソルは描画結果でありプラットフォーム状態なので、
  テストプラットフォームでは意味のある検証ができない）
- 実機確認は macOS で目視。境界（トリムエッジ 6px、ポートとノードの境目、
  パスハンドルのヒット半径）を跨いで確認する

## 他の単位との関係

- **`OVL-1`（Viewer オーバーレイ機構の抽出、フェーズ C）**: `PTR-5` が入れる
  hover 判定は Viewer の入力経路に乗る。`OVL-1` は挙動不変のリファクタなので
  ヒント写像を維持する義務がある。順序が逆でも成立するが、`OVL-1` が後に来る
  ほうがヒットテストの一元化（`handles()` に集約）と同時にヒントも 1 箇所へ
  寄せられる
- **`OVL-3` / `MED-APP-21`（`shape_node_bounds` の type_key match 廃止）**:
  `PTR-5` の hover は同じ bounds 計算を使う。`OVL-3` が実データ由来の bounds に
  切り替えた時点でヒント境界も実形状に一致するようになる。**`PTR-5` は
  独自の bounds 計算を追加しない**
- **`MED-APP-15`（Hand / Zoom が dead UI）**: 上記「機能が無いものに
  カーソルを付けない」の対象。機能実装と同じ単位でカーソルも入れる
- **フェーズ C3 の `MED-UI-*`（パネル 1 回あたりのコスト）**: 本計画は notify を
  増やさない設計なので前提を悪化させない。逆に C3 が Timeline の行仮想化を
  入れても、ヒント判定は既存ヒットテスト経由なので影響を受けない
- **`MED-APP-13`（Timeline の行レイアウト走査が 4 箇所）**: 5 箇所目を作らない。
  `PTR-3` は `row_at_content_y` / `bar_hit` を呼ぶだけ

## 非対象

- **独自カーソル画像**、**カーソルの自前描画**、**任意タイミングの OS カーソル
  非表示**。`gpui-ce` が提供していない（上記「使える機構」参照）
- **gpui-component `Button` のカーソル**。ライブラリが `cursor_default()` を
  明示している。ravel 側ラッパーの導入は別途判断する
- **Hand / Zoom ツールのカーソル**と**bbox リサイズハンドルのカーソル**。
  機能が無い（`MED-APP-15` / `OVL-*`）
- **ツールごとのカスタムポインタ表現**（ペン先の形など）。独自カーソル画像が
  必要なので不可
- **ドラッグ中の数値 HUD**（座標・サイズの追従表示）。カーソルではなく
  オーバーレイなので `viewer-overlay-manipulator-plan.md` の担当
