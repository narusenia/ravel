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
「編集中の体感」を最も悪化させている要因。

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
