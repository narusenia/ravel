# TASK-010: ハードウェアデコーダ統合
- **マイルストーン**: MS2 Media Pipeline
- **関連要件**: REQ-MEDIA-001, REQ-GPU-001
- **規模**: L
- **依存タスク**: TASK-009, TASK-005

## 概要
各プラットフォームのハードウェアデコーダ（VideoToolbox, NVDEC, AMF）をRavelのメディアパイプラインに統合する。HWデコーダ出力をwgpuテクスチャへゼロコピーで転送し、GPUメモリ上でのシームレスな映像処理を実現する。HWデコードが利用不可な環境ではソフトウェアデコードへ自動フォールバックする。

## 実装ステップ
1. macOS VideoToolbox統合（ゼロコピーGPU出力）
2. Windows NVDEC/AMF統合
3. GPUメモリインターオペ（HWデコーダ出力 → wgpuテクスチャ）
4. ソフトウェアデコードへの自動フォールバック
5. プラットフォーム抽象化トレイト定義
6. パフォーマンスベンチマーク（HW vs SWデコード比較）

## 対象コンポーネント
- `crates/ravel-media/` (HWデコーダバックエンド)
- `crates/ravel-gpu/` (GPUメモリインターオペ)

## 完了条件
- [x] macOSでVideoToolboxによるHWデコードが動作
- [x] WindowsでNVDEC/D3D11VAによるHWデコードが動作（コード実装済み、Windows CIで検証）
- [ ] HWデコーダ出力がwgpuテクスチャへゼロコピー転送される → 将来タスク（GitHub Issue参照）
- [x] HW非対応環境でSWデコードへ自動フォールバック
- [x] プラットフォーム抽象化（HwBackend enum + HwDeviceContext RAII）により統一的なAPIで利用可能
- [x] HW vs SWのデコードパフォーマンスベンチマーク（criterion）を追加

## 実装メモ
- HWデコード → CPU readback（`av_hwframe_transfer_data`）→ SWScale RGBA f32 → `FrameBuffer` のパス
- ゼロコピー GPU interop（VideoToolbox → Metal → wgpu texture）は別タスクとして切り出し
- デコーダコンテキストのキャッシュ化により、フレームごとの再生成を排除（SW/HW共通の性能改善）
- `get_format` コールバックでHWピクセルフォーマットをネゴシエーション
