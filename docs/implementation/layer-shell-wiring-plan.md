# レイヤー殻の未配線フィールド 実装計画

> **Status**: Planned — 2026-07-27

対象: `Layer` 殻に定義済みだが評価に繋がっていない `track_matte` と
`time_remap` を配線する。関連要件: REQ-LAYER、REQ-CORE-001、REQ-CORE-007。

## 問題

`Layer` にフィールドが**存在し、永続化もされるのに、`compile.rs` が
一切扱っていない**。

| フィールド | 定義 | compile.rs での扱い | 設定 UI |
|---|---|---|---|
| `adjustment: bool` | ✅ | ✅ `comp.merge.adjustment` として実装済み | ✅ |
| `track_matte: Option<TrackMatte>` | ✅（`TrackMatteKind::{Alpha, Luma}` まで） | ❌ **出現しない** | ❌ |
| `time_remap: Option<AnimationChannel>` | ✅ | ❌ **出現しない** | ❌ |
| `parent: Option<LayerId>` | ✅ | ✅ P/R/S の継承が効く | ❌ **無い**（単位 5） |

`adjustment` が実装済みなので、評価側で取り残されているのは 2 つ。
`parent` は**逆に評価だけが動いていて設定手段が無い**ので、同じ「宣言と
実装のずれ」として本計画が引き受ける（単位 5）。

これは「未実装」より悪い状態で、**データモデルと永続化が先行しているため
UI から設定できてしまうと「設定したのに効かない」バグに見える**。
Outliner / Properties がこれらを露出した瞬間に顕在化する。

## 決定事項

### `time_remap` はスコープ軸を使わない

レイヤーの時間リマップは「このレイヤーのネットワークを別の時刻で評価する」
もので、**下流と上流が同時に異なる時刻を要求しない**（レイヤー全体が
リマップ後の時刻で評価される）。したがって `ctx` を差し替えて評価するだけで、
`evaluation-scope-plan.md` の `PathSegment::TimeShift` は要らない。

`TimeShift` が要るのは FX-5 のノードレベルのタイムリマップ（下流が `f` の
まま上流だけ `f'`）。**両者を混同しない。**

### `time_remap` は連続時間チャネルに依存する

`time_remap: AnimationChannel` は「出力フレーム → 入力時刻」の写像。
現状の `AnimationChannel::evaluate(frame: u64)` では整数フレームに丸められ、
**スローモーションが階段状になる**。

`motion-blur-plan.md` の単位 1（連続時間化）を前提にする。

### `track_matte` はマット元レイヤーを合成から外す

慣例どおり、マットとして使われたレイヤーはコンポジットに現れない。
`compile.rs` はマット元レイヤーの出力をマット対象の合成チェーンへ
分岐させ、マット元自身の Merge は生成しない。

`adjustment` が「下のスタックを自分のネットワークへ流す」ために既に
分岐を作っている（`compile.rs:255-258`）ので、その形を踏襲する。

### マットの対象は直下のレイヤー 1 枚

AE と同じく「マット元の 1 つ下のレイヤーに適用」。任意レイヤーを
名前で指定する方式は採らない（レイヤー順の入れ替えで壊れる）。

## 実装単位

### 単位 1: `time_remap` の配線

- `compile.rs` の時間変換段で `time_remap` を評価し、ネットワーク境界へ
  渡す `EvalContext` の時刻を差し替える。
- リマップ結果がレイヤーの `in_frame`..`out_frame` の外を指した場合の
  挙動を定義する（クランプ）。

**完了条件**

- 恒等リマップで挙動が変わらないテスト。
- 2 倍速 / 0.5 倍速のテスト。
- **分数時刻でメディアが補間される**テスト（連続時間化の効果検証）。
- 範囲外を指したときのクランプテスト。
- コンパイルトポロジのスナップショットテスト。

### 単位 2: `track_matte` の配線

- `compile.rs` にマット合成段を追加。`Alpha` / `Luma` の 2 種。
- マット元レイヤーを通常の合成から除外。
- 反転（invert）は `TrackMatteKind` に含まれていないので**追加する**
  （`AlphaInverted` / `LumaInverted`）。永続化のマイグレーションを伴う。

**完了条件**

- Alpha マットで対象がマット元のアルファに従って抜けるゴールデンテスト。
- Luma マットのゴールデンテスト。
- 反転マットのゴールデンテスト。
- **マット元レイヤーが合成に現れない**ことのテスト。
- マット元が存在しない（最上位レイヤーに設定）場合にエラーにならず
  マット無しとして扱われるテスト。
- コンパイルトポロジのスナップショットテスト。

### 単位 3: UI 露出

- Outliner / Properties にマットとタイムリマップの設定。
- タイムリマップはカーブエディタで編集できるようにする
  （既存の `AnimationChannel` UI に載る）。
- ロケール。

**完了条件**

- UI から設定して評価結果が変わるテスト。
- `mise run check` 通過。

### 単位 5: `parent` の設定 UI

`track_matte` / `time_remap` と**逆向きの取り残し**。`parent: Option<LayerId>`
（`composition/mod.rs:205`）は評価では効く（親の P/R/S を継承する）のに、
**設定する UI がどこにも無い**。Properties のレイヤー節は timing / transform /
opacity / blend / adjustment だけで、Timeline にも Parent 列は無い。
Outliner は親子を表示するが「表示のみ、D&D 不可」（`ui-spec.md`）。

`roadmap.md` の基準 4（評価はできるが編集できない）に該当し、**残っている
実装量に対して得られる機能が最大**の類。

- Properties のレイヤー節に Parent ドロップダウンを追加する。候補は同一
  コンポジションの他レイヤー + `(none)`
- **循環を作る候補は列挙から外す**（自分自身と、自分を先祖に持つレイヤー）。
  評価側には既に循環で停止する扱いがある（`viewer.rs` の
  `parent_cycles_terminate` テスト）が、UI で作れないようにするのが本筋
- 親の付け替えは既存の殻編集経路（Document 変更 + `InvalidationHint::Structural`）
  に乗せ、1 操作 1 undo
- 親を持つレイヤーの表示（Outliner のインデント）は既存のまま変えない
- Viewer の親子リンク線は `viewer-overlay-manipulator-plan.md` の単位 7 が持つ。
  **この単位は設定手段だけ**

**完了条件**

- ドロップダウンで親を設定すると子の変換が親に追従するテスト
- 循環になる候補が列挙されないテスト（自身・子孫を除外）
- 親を削除したときに子の `parent` が `None` に戻る（または解決不能な
  `LayerId` を残さない）テスト
- 設定・解除が 1 undo で戻るテスト
- ロケール（en / ja）に Parent 行の文字列が入っていること

### 単位 4: 文書更新

- `docs/specifications/data-model.md`: 2 フィールドの評価時挙動。
- `docs/implementation/plan.md`: 該当箇所。
- `parent` の設定 UI（単位 5）を `docs/ui-impl-status.md` の Properties 表へ。

## 検証

- ゴールデンは CPU 経路。
- **単位 2 の「マット元が合成に現れない」テストが重要**。これを落とすと
  マット元が二重に見える状態になり、目視では気づきにくい。

## 非対象

- **任意レイヤーをマット元に指定する方式**。直下 1 枚のみ。
- **マットの部分適用**（マット元の一部だけ使う）。
- **`time_remap` のフレームブレンド**。リマップ結果が分数フレームでも
  隣接フレームのブレンドはしない。`effects-library-plan.md` の FX-5 が
  ノードレベルで扱う。
