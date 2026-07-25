# TASK-005: wgpu GPU計算パイプライン基盤
- **マイルストーン**: MS1 Foundation
- **関連要件**: REQ-GPU-001, REQ-GPU-002, REQ-INFRA-001
- **規模**: L
- **依存タスク**: TASK-003

## 概要

wgpuベースのGPU計算パイプライン基盤を構築する。デバイス初期化、コンピュートシェーダのディスパッチ、テクスチャプール管理、シェーダモジュール管理を実装し、ノード評価のGPU処理を可能にする。macOS（Metal）とWindows（D3D11）の両バックエンドで動作確認する。

## 実装ステップ

1. **wgpuデバイス/キュー初期化**
   - `wgpu::Instance`作成（バックエンド自動選択: Metal on macOS, D3D11 on Windows）
   - `Adapter` → `Device` + `Queue`の取得
   - デバイス機能/制限の確認とログ出力
   - GPUIとのGPUコンテキスト共有方針の調査・実装（GPUIのwgpuインスタンスを再利用できるか検証）

2. **コンピュートシェーダディスパッチパイプライン**
   - `ComputePipeline`作成のラッパー
   - バインドグループ（テクスチャ、ユニフォームバッファ）のレイアウト定義
   - ディスパッチヘルパー: ワークグループサイズ計算、テクスチャサイズに基づく自動ディスパッチ
   - `GpuTask`トレイト: `fn dispatch(&self, encoder: &mut CommandEncoder, ctx: &GpuContext)`
   - サンプルシェーダ（カラー反転、ブラー等）で動作確認

3. **テクスチャプール管理**
   - テクスチャプール: 同サイズ・同フォーマットのテクスチャを再利用
   - アロケーション: 要求サイズ/フォーマットに一致する空きテクスチャを返却、なければ新規作成
   - リリース: 使用完了テクスチャをプールに返却
   - LRUベースの自動解放（VRAM使用量が閾値を超えたら古いテクスチャを解放）
   - VRAM使用量の追跡とログ出力

4. **シェーダモジュール管理**
   - ビルトインシェーダ: `include_str!`でWGSLソースをバイナリに埋め込み
   - ランタイムコンパイル: WGSLソースから`ShaderModule`を作成
   - コンパイル済みモジュールのキャッシュ（HashMap by shader source hash）
   - コンパイルエラーのユーザーフレンドリーなレポート

5. **デバッグビルドでのシェーダホットリロード**
   - `#[cfg(debug_assertions)]`でホットリロードを有効化
   - `notify`クレートでシェーダファイルの変更を監視
   - 変更検出時にシェーダ再コンパイル → パイプライン再作成
   - リリースビルドではビルド時コンパイル済みを使用（起動速度確保）

6. **GPU-CPUデータ転送ユーティリティ**
   - GPU → CPU: テクスチャ読み戻し（`buffer.map_async`ベース）
   - CPU → GPU: テクスチャアップロード（`queue.write_texture`）
   - 転送の非同期化（コールバックベース）
   - ゼロコピー可能なケースの判定

7. **Metal (macOS) / D3D11 (Windows) バックエンド検証**
   - macOSでMetalバックエンドでのコンピュートシェーダ動作確認
   - Windows CIでD3D11バックエンドでのビルド確認（GPU無し環境ではスキップ）
   - バックエンド差異の抽象化（テクスチャフォーマット制約等）

## 対象コンポーネント

- `crates/ravel-gpu/src/lib.rs` — GPUモジュールエントリ
- `crates/ravel-gpu/src/device.rs` — wgpuデバイス初期化・管理
- `crates/ravel-gpu/src/compute.rs` — コンピュートパイプライン・ディスパッチ
- `crates/ravel-gpu/src/texture_pool.rs` — テクスチャプール管理
- `crates/ravel-gpu/src/shader.rs` — シェーダモジュール管理・ホットリロード
- `crates/ravel-gpu/src/transfer.rs` — GPU-CPU転送ユーティリティ
- `crates/ravel-gpu/src/shaders/` — ビルトインWGSLシェーダ

## 完了条件

- [ ] wgpuデバイス/キューが初期化される
- [ ] コンピュートシェーダのディスパッチが動作する（サンプルシェーダで確認）
- [ ] テクスチャプールがアロケーション/リリース/自動解放を行う
- [ ] ビルトインシェーダがバイナリに埋め込まれている
- [ ] ランタイムでWGSLシェーダがコンパイル・実行される
- [ ] デバッグビルドでシェーダホットリロードが機能する
- [ ] GPU→CPU、CPU→GPUのデータ転送が動作する
- [ ] macOS（Metal）でコンピュートシェーダが動作する
- [ ] Windows CI上でD3D11バックエンドのビルドが通る
- [ ] VRAM使用量がログ出力される
