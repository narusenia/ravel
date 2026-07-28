# [HIGH-06] パラメータ編集ごとに GPU プロセッサを再構築（naga 再検証 + パイプライン再作成）

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | perf |
| 領域 | ravel-gpu / シェーダ・パイプライン, ravel-app / eval_hooks |
| 該当 | `crates/ravel-gpu/src/shader.rs:133-137`, `crates/ravel-gpu/src/compute.rs:52-88`, `crates/ravel-app/src/eval_hooks.rs:73-113` |

## 現状

`GpuEvalHooks::sync` は `InvalidationHint::Params` で編集ノードごとに `processor_for_node` を呼ぶ。
GPU ノードのコンストラクタは `ShaderManager::compile_source` を呼び、そこで

- `validate_wgsl`（naga の完全パース + 検証）が**ハッシュキャッシュ参照より前**に走る
  → モジュールキャッシュが検証コストを一切削減していない
- 続いて `ComputePipeline::new` が BindGroupLayout / PipelineLayout / ComputePipeline を新規作成
  （ドライバ側コンパイル）

`InvalidationHint::Structural` では、ドキュメント内の全レイヤーネットワークの全 GPU ノードに対して
これを実行する。

`lib.rs:86-89` の設計コメント「パラメータ編集は dirty マークのみで再構築不要」と実装が矛盾している。
再構築が本当に必要なのは、ノード状態をキャプチャする `from_node` 系プロセッサのみ。

## 影響

ブラー半径スライダーのドラッグ = 変更イベントごとにコンピュートパイプライン再コンパイル。

**当初この項目を「編集中の体感を最も悪化させている要因」と書いたが、測定はそれを支持しない。**
`perf-baseline.md`「RESP-3 完了時」の実測では、変更前でも
`register_processors`（全プロセッサ再登録 + パイプライン再生成 + naga 再検証）は
tick の約23%（0.31 ms / 1.31 ms）で、残りは `gpu_upload` と GPU ノードの
`node_process`。編集時の体感の主因は第2段
（[HIGH-04](HIGH-04-per-frame-blocking-readback.md) /
[HIGH-05](HIGH-05-shell-chain-cpu-per-pixel.md) の CPU↔GPU 往復と
シェル合成の CPU per-pixel）側にある。

また `ravel-gpu/src/lib.rs:86-89` に「パラメータ編集は dirty マークのみで再構築不要」という
設計コメントがあると書いたが、現在のソースには存在しない。
`InvalidationHint::Params` の doc（`eval_service.rs:38-40`）は逆に
「該当ノードのプロセッサだけ再構築する」と書いており、実装と一致している。
矛盾していたのは設計コメントではなく、その再構築が GPU ノードでは無意味だという事実。

## 修正方針

1. `sync` でノード状態をキャプチャしないプロセッサ（GPU 系）の再登録をスキップ
2. `compile_source` の検証をソースハッシュキャッシュの後ろへ移動
3. `ComputePipeline` を (シェーダハッシュ, エントリポイント) キーで共有レジストリにキャッシュ
   → 同種 N ノードが1パイプラインを共有

## 検証

- スライダードラッグ中にパイプライン作成回数が 0 であることをカウンタで確認
- 構造編集時の再コンパイル回数がノード種類数に比例（ノード数に比例しない）ことを確認

## 関連

- [HIGH-07](HIGH-07-document-changed-cascade-per-mouse-move.md) — 編集ティックあたりの UI 側コスト
- [medium/gpu-nodes.md](../medium/gpu-nodes.md)
