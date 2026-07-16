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

`crates/ravel-app/src/widgets/scrub_input.rs` 新設。drag-only
（クリック編集は #41 解決後）。

- 表示: ラベル + 数値。ホバーで左右リサイズカーソル。
- ドラッグ: mouse_down で開始し **要素の on_mouse_move / on_mouse_up_out で
  window 外も追跡**（NodeEditor の DragMode と同じ堅牢化）。
- 感度: `ui range 幅 / 200px` を 1px あたりの増分とする
  （固定 px/step にしない）。Shift = 10x、Cmd/Ctrl = 0.1x。
- clamp: **hard range**（スクラブで ui range を超えられる。ui は感度と
  Slider 表示のみに使う）。
- イベント: `Change(f32)`（ドラッグ中、ライブ反映・undo 積まない）と
  `Commit(f32)`（mouse-up、undo push）を分離。
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
  操作感は手動確認。
- Phase 3: ドラッグ 1 回で undo 1 段になるヘッドレステスト
  （Change×N + Commit → undo 1 回で元値）。
- `mise run check` 緑、ravel-review PASS。

## リスク・注意

- `sections_for_node` のシグネチャ変更は ravel-ui の公開 API 変更 —
  呼び出しは properties.rs と tests のみで影響小。
- ScrubInput は gpui-component 非依存で書く（custom UI lib 方針に整合）。
- クリックしてキーボード入力するモードは #41（IME/テキスト入力）解決まで
  スコープ外。
- Layer プロパティ（sections_for_layer）は registry 外 — レンジは
  レイヤー側ビルダーで直接指定（transform/opacity のみ、opacity 0..=1）。
