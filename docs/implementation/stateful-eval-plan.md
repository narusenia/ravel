# ステートフル評価と sim キャッシュ実装計画（REQ-CORE-011）

> **Status**: Planned — 2026-07-27

対象要件: REQ-CORE-011（優先度 Must）。関連: REQ-CORE-002（Hybrid Pull）、
REQ-CORE-005（バックグラウンド評価）、REQ-CORE-006（三層キャッシュ）。

`particle-plan.md`（REQ-MOGRAPH-002）の前提。パーティクルが最初の
利用者だが、トレイル・物理・時間積分系すべてが同じ機構に乗るため、
評価エンジンの改修として独立させる（AGENTS.md の design gate が
「evaluation」のサブシステム改修を明示的に挙げている）。

**前提**: `evaluation-scope-plan.md` の単位 1（`PathSegment` の
スコープ次元）。本計画・FX-5・グラフ内反復が同じキャッシュ制約に
当たっており、軸を先に共通化しないと機構が 3 つに分裂する。

設計は `docs/specifications/procedural-geometry.md:88-127` に既にある。
本計画はそれを実装単位へ落とすもので、**設計を変更しない**。

## 問題

Hybrid Pull（REQ-CORE-002）は「フレーム t の値は t だけから決まる」前提で
組まれている。`Evaluator` のキャッシュ（`eval.rs:457`）は
`NodeKey`（path + node）から `CacheEntry` への map で、フレームが変われば
再計算するだけ。**前フレームの結果に依存するノードを表現できない。**

`procedural-geometry.md` の「既存コードへの影響」表でも
`ravel-core/src/eval.rs` の sim キャッシュ統合は 🔲 未着手。

## 決定事項

すべて `procedural-geometry.md` の既存記述に従う。ここでは実装上の
含意だけ補う。

### `StatefulProcessor` は `NodeProcessor` と別トレイト

```rust
pub trait StatefulProcessor {
    type State: Send + Sync;
    fn initial(&self, ctx: &EvalContext, inputs: &Inputs) -> Self::State;
    fn step(&self, prev: &Self::State, ctx: &EvalContext, inputs: &Inputs)
        -> Self::State;
}
```

`Evaluator` に登録する際は型消去したアダプタで包み、
`Arc<dyn NodeData>` を出す通常ノードとして下流から見えるようにする。
下流のノードはステートフルかどうかを知らなくてよい。

### sim キャッシュは別 map。ただしキー型は共有する

既存の `cache: HashMap<NodeKey, CacheEntry>` とは分ける。

```text
sim_cache: HashMap<NodeKey, SimTrack>
SimTrack { start_frame: u64, states: Vec<Arc<dyn Any + Send + Sync>>, input_hash: u64 }
```

フレーム連続区間として持つ。フレーム t の pull で
`[last_cached+1, t]` を順に `step` して埋める。

**`NodeKey` を共有するのが条件**（`evaluation-scope-plan.md` の決定）。
独自のキー型を作らない。sim を汎用キャッシュに載せない理由は
「1 ノードに値が 1 つしか置けないから」ではなく、**フレーム連続区間が
順序に意味を持つ系列で、逐次充填という別のアクセスパターンを持つ**から。
汎用キャッシュに載せるとスクラブのたびに数百エントリが LRU を汚す。

**バイト予算は `cache-plan.md` の `CacheBudget` から受け取る。** sim は
追い出されると再計算が O(フレーム数) で他の層と非対称なので、予算内に
**保護枠**（既定で総額の 25%、設定可変）を持ち、通常のフレームキャッシュの
圧力では削られない。枠を超える長期保持は暗黙 LRU で守るのではなく、
将来の明示キャッシュノードに寄せる（同計画の「将来方向」）。
`SimTrack` 内部の退避は「区間の先頭側を捨てない」という sim 固有の規則に
なる（再生の先頭が要るため）。

「同一ノードの複数結果を保持できない」という制約そのものは
`evaluation-scope-plan.md` が `PathSegment` の拡張で解く。
本計画・FX-5・グラフ内反復の 3 つが同じ制約に当たっているので、
**回避策を各自で持たない**。

### 無効化は入力ハッシュの全破棄（v1）

上流サブグラフの構造/パラメータハッシュを `SimTrack.input_hash` に記録し、
変化したら**全区間破棄**。キーフレーム変化の影響開始フレーム以降のみ
破棄する最適化（spec の v2）は本計画では行わない。

既存の `Evaluator` は path ベースの scope 無効化を持つ
（`eval.rs:490-511`）。sim キャッシュも同じ経路でまとめて落とす。

### 多段接続は 1 段に制限

spec の制約どおり。sim の下流に sim を繋いだ場合は評価エラーにする
（黙って壊れた結果を出さない）。

### 前方ジャンプは暫定表示

`[last+1, t]` の充填が長い場合、最後のキャッシュ済み状態を返しつつ
バックグラウンドで埋める。既存の `EvalService` は 1 リクエスト =
1 評価なので、**充填の途中経過を返す仕組みが要る**。
`EvalUpdate` に「暫定値」フラグを足す。

なお `attribute-spreadsheet-plan.md` の単位 1 が `EvalRequest` /
`EvalUpdate` を複数ターゲット化する。**どちらが先でも構わないが、
両方が同じ型を触る**ので実施順を決めてから着手する。

## 実装単位

### 単位 1: `StatefulProcessor` と sim キャッシュの骨格

- トレイト定義と型消去アダプタ。
- `Evaluator` に `sim_cache` を追加。区間充填ロジック。
- 決定性の担保: `step` は `ctx` と `prev` と入力のみに依存。
  乱数は `seed` パラメータ + `id` 属性由来のハッシュのみ
  （`Date` / `Math.random` 相当の禁止は既存の lint 対象）。

**完了条件**

- カウンタ的なテスト用ステートフルノードで、frame 10 の pull が
  10 回 `step` することを検証。
- frame 10 → frame 5 → frame 10 でキャッシュから即返る（`step` 回数が
  増えない）テスト。
- 同一シード・同一入力で 2 回評価して同一結果になるテスト。
- sim の下流に sim を繋いで評価エラーになるテスト。

### 単位 2: 無効化

- 上流サブグラフの構造/パラメータハッシュ計算。
- パラメータ変更 → 全区間破棄 → 再充填のテスト。
- 既存の scope 無効化経路（`drop_scope_owner_caches`）との統合。

**完了条件**

- パラメータ変更で `SimTrack` が破棄されるテスト。
- **無関係なノードの変更では破棄されない**テスト（過剰無効化の回帰）。
- レイヤーネットワーク内のステートフルノードが、シェル側の編集で
  誤って破棄されないテスト。

### 単位 3: 暫定表示とバックグラウンド充填

- `EvalUpdate` に暫定フラグ。
- 前方ジャンプ時に最後のキャッシュ状態を即返し、充填を継続。
- 充填は評価スレッドプールで行い UI を塞がない（REQ-CORE-005）。

**完了条件**

- frame 0 から frame 300 へジャンプしたとき、最初の `EvalUpdate` が
  暫定フラグ付きで即返るテスト。
- 充填完了後に確定値が publish されるテスト。
- 充填中に別のリクエストが来たときの挙動（充填を捨てる）のテスト。

### 単位 4: 文書更新

- `docs/specifications/procedural-geometry.md`: sim キャッシュ節の
  「未着手」表記を更新。
- `docs/requirements/REQ-CORE.md`: REQ-CORE-011 の受入条件を更新。

## 検証

- すべてヘッドレス（`ravel-core`）。GPU 不要。
- **決定性テストを最優先**。sim は非決定性が混ざると再現しないバグの
  温床になるので、同一入力 2 回評価の一致を全単位で回す。
- 過剰無効化はスクラブ体感の劣化として現れるので、単位 2 の
  「無関係な変更で破棄されない」テストを回帰として残す。

## 非対象

- **影響開始フレーム以降のみ破棄する最適化**（spec の v2）。
- **sim キャッシュのディスク層へのスピル**（REQ-CORE-006 の RAM 層まで）。
- **多段のステートフル接続**。
- **GPU でのシミュレーション**。CPU SoA（rayon）のみ
  （`procedural-geometry.md` の GPU 方針に従う）。
- **グラフ内反復**（REQ-CORE-013）。spec が v1 で採用しないと明記。
