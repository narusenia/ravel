# パラメータレンジ + ScrubInput 実装計画

対象: Properties パネルの数値パラメータ編集。
関連 Issue: #41（テキスト入力 — 本計画では繰延）。

## 問題

1. パラメータにレンジのメタデータ源がない。`sections_for_node` は常に
   `range: None` を返し、Slider は fallback の `-10..=10` に固定される —
   blur radius が 10 までしか動かせない、負であってはならない値
   （width、radius、count）が負に振れる。
2. Slider ドラッグは `SliderEvent::Change` ごとに
   `apply_property_change` → `undo_stack.push` が走り、1 ドラッグで
   undo スナップショットが大量に積まれる。
3. Slider はモーショングラフィックスの数値編集 UI として弱い
   （AE/Houdini はドラッグスクラブが標準）。旧 `feat/scrub-value-input`
   ブランチの ScrubInput は要素ローカル mouse_move の追跡切れ・
   テキスト編集の IME 問題（#41）で失敗しており、参考にしない。

## ターゲット構成

### Phase 1: レンジメタデータ（ravel-core → ravel-ui）

```rust
// registry/mod.rs
pub struct ParamRange {
    pub hard: RangeInclusive<f32>,  // clamp 境界（真の min/max）
    pub ui: RangeInclusive<f32>,    // 操作レンジ（スクラブ感度の基準）
}
NodeTemplate { param_ranges: HashMap<String, ParamRange>, .. }
    .with_param_range(key, hard, ui)   // ui ⊆ hard を debug_assert
NodeRegistry::param_range(type_key, param_key) -> Option<&ParamRange>
```

- 全ビルトインの数値パラメータ（Float/Int）にレンジ定義。
  例: blur.radius hard 0..=500 / ui 0..=50、merge.mix 0..=1 / 0..=1、
  shape.\* の width/radius hard 0..=100000 / ui 0..=500、
  scatter.\* count hard 1..=100000 / ui 1..=200、seed 0..=i32::MAX / 0..=1000。
- `PropertyField::Float/Int` に `ui_range` を追加（既存 `range` は hard に
  意味づけ）。`sections_for_node(node, registry)` にシグネチャ変更し、
  テンプレートからレンジを引く（registry にないノードは従来どおり None）。
- Int は f32 レンジを cast して使う。

### Phase 2: ScrubInput ウィジェット（ravel-app）

`crates/ravel-app/src/widgets/scrub_input.rs` 新設。ドラッグスクラブ +
クリックでテキスト編集（実装時に前倒し — 下記参照）。

- 表示: ラベル + 数値。ホバーで左右リサイズカーソル。
- ドラッグ: gpui の `on_drag` / `on_drag_move`（window レベル追跡）+
  `on_mouse_up` / `on_mouse_up_out`（gpui-component Slider と同じ
  堅牢化パターン。旧実装の要素ローカル mouse_move 追跡切れを回避）。
- 感度: `ui range 幅 / 200px` を 1px あたりの増分とする
  （固定 px/step にしない）。Shift = 10x、Cmd/Ctrl = 0.1x。
- clamp: **hard range**（スクラブで ui range を超えられる。ui は感度に
  のみ使う）。
- イベント: `Change(f32)`（ドラッグ中、ライブ反映・undo 積まない）と
  `Commit(f32)`（mouse-up、undo push）を分離。始点に戻して離した場合は
  Commit を発行しない（no-op undo を積まない）。
- **クリックでテキスト編集**: 当初 #41 解決後に繰延の予定だったが、
  `gpui_component::input::Input`（`EntityInputHandler` 実装）が正規の
  テキスト入力経路を通るため前倒しで実装。クリック（ドラッグなし）で
  Input に切替・全選択、Enter/フォーカス喪失で確定（パース → hard clamp）、
  パース不能は元値へ復元。Esc キャンセルは未対応。
  ScrubInput 本体は gpui-component 非依存だが、編集モードのみ Input を
  借用する（custom UI lib 移行時に独自 InputField へ差し替え、#41）。
- Float は Slider → ScrubInput 置換。Int も ScrubInput（step=1、整数丸め）。

### Phase 3: undo 粒度の修正（ravel-app）

- `PropertyChanged` に `commit: bool` を追加。
- `NodeEditorPanel::apply_property_change`: `commit == false` は graph
  差し替え + processor 同期 + Viewer 再評価のみ（undo push しない）、
  `commit == true` で `undo_stack.push`。
- 値は適用時に registry の hard range で clamp（PropertyChanged 経由の
  将来の入力源にも効く）。

## 完了条件

- Phase 1: ui ⊆ hard の検証テスト、全ビルトイン数値パラメータにレンジが
  ある網羅テスト、`sections_for_node` がレンジを反映するテスト。
- Phase 2: スクラブ増分・modifier・clamp のロジックを純関数
  （`scrub_value(start, dx, range, modifiers)`）に切り出しユニットテスト。
  ジェスチャ単位のイベント発行（Change×N + Commit×1、戻し操作は Commit
  なし）は TestAppContext テスト。操作感・テキスト編集のフォーカス/IME
  挙動は手動確認（編集モードの interaction テストは未整備 — 既知の gap）。
- Phase 3: ドラッグ 1 回で undo 1 段になるヘッドレステスト
  （Change×N + Commit → undo 1 回で元値）。
- `mise run check` 緑、ravel-review PASS。

## リスク・注意

- `sections_for_node` のシグネチャ変更は ravel-ui の公開 API 変更 —
  呼び出しは properties.rs と tests のみで影響小。
- ScrubInput のスクラブ部は gpui-component 非依存（custom UI lib 方針に
  整合）。編集モードは暫定的に gpui-component Input を借用（#41 で独自化）。
- undo 粒度テストは NodeEditorPanel 構築に GPU アダプタが要る（既存の
  GPUI 統合テストと同条件）。純ロジック部は adapter 不要。
- Layer プロパティ（sections_for_layer）は registry 外 — レンジは
  レイヤー側ビルダーで直接指定（transform/opacity のみ、opacity 0..=1）。
