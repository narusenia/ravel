# TASK-028: OpenColorIO統合 + GPU LUT
- **マイルストーン**: MS6 Pro Features
- **関連要件**: REQ-RENDER-003
- **規模**: L
- **依存タスク**: TASK-005

## 概要
OCIO C++ライブラリのFFIバインディング、.ocioコンフィグファイル読み込み、色空間変換（入力→作業→表示→出力）、OCIOトランスフォームからのGPU 3D LUTベイク、wgpuシェーダによるビューワでのLUT適用、色空間セレクタUI、ビューワ（REQ-UI-004）およびスコープとの統合を実装する。ACES、Rec.709、sRGB、Rec.2020をサポートし、LUT再生成はコンフィグ変更時のみ（フレーム毎ではない）。

## 実装ステップ
1. OCIO C++ライブラリのFFIバインディングセットアップ
2. .ocioコンフィグファイル読み込み実装
3. 色空間変換実装（入力→作業→表示→出力）
4. OCIOトランスフォームからのGPU 3D LUTベイク実装
5. wgpuシェーダによるビューワでのLUT適用実装
6. 色空間セレクタUI実装
7. ビューワ（REQ-UI-004）およびスコープとの統合
8. ACES、Rec.709、sRGB、Rec.2020サポート
9. LUT再生成をコンフィグ変更時のみに制限（フレーム毎再生成の回避）

## 対象コンポーネント
- `crates/ravel-color/` (カラーマネジメント)
- `crates/ravel-color/src/ocio/` (OCIO FFIバインディング)
- `crates/ravel-color/src/lut/` (LUT管理)
- `crates/ravel-gpu/src/lut_shader/` (GPU LUTシェーダ)
- `crates/ravel-ui/src/viewer/color_space/` (色空間セレクタUI)

## 完了条件
- [ ] OCIO C++ライブラリのFFIバインディングが動作する
- [ ] .ocioコンフィグファイルが正しく読み込まれる
- [ ] 入力→作業→表示→出力の色空間変換パイプラインが動作する
- [ ] OCIOトランスフォームからGPU 3D LUTがベイクされる
- [ ] wgpuシェーダでビューワにLUTが適用される
- [ ] 色空間セレクタUIが動作する
- [ ] ACES、Rec.709、sRGB、Rec.2020がサポートされている
- [ ] LUT再生成がコンフィグ変更時のみに限定されている
