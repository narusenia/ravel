# TASK-009: FFmpegデコード/エンコード統合
- **マイルストーン**: MS2 Media Pipeline
- **関連要件**: REQ-MEDIA-001
- **規模**: L
- **依存タスク**: TASK-003

## 概要
FFmpegをRavelのメディアパイプラインに統合し、主要な映像/音声/画像フォーマットのデコード・エンコード機能を提供する。LGPL準拠のためダイナミックリンク方式を採用し、`ffmpeg-next`クレートを通じてFFmpegライブラリを利用する。

## 実装ステップ
1. FFmpegダイナミックリンク設定（LGPL準拠）
2. ビデオデコーダ実装（`ffmpeg-next`クレート使用）
3. オーディオデコーダ実装
4. エンコーダパイプライン実装
5. `MediaReader` / `MediaWriter`トレイト定義
6. フォーマット自動検出の実装
7. H.264, H.265, AV1, ProRes, DNxHRデコード対応
8. MP4, MOV, MKV, WebMコンテナ対応
9. イメージシーケンス対応（EXR, PNG, TIFF, DPX）
10. サンプルメディアファイルを用いた結合テスト

## 対象コンポーネント
- `crates/ravel-media/` (新規またはメディア関連クレート)
- `crates/ravel-core/` (トレイト定義)

## 完了条件
- [x] FFmpegがダイナミックリンクで正しくリンクされLGPL準拠
- [x] `MediaReader`トレイトで主要コーデック（H.264, H.265, AV1, ProRes, DNxHR）のデコードが動作
- [x] `MediaWriter`トレイトでエンコードパイプラインが動作
- [x] MP4, MOV, MKV, WebMコンテナの読み書き対応
- [x] EXR, PNG, TIFF, DPXイメージシーケンスの読み込み対応
- [x] フォーマット自動検出が正しく機能
- [x] サンプルメディアを用いた結合テストがパス
