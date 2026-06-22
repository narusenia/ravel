# TASK-027: オーディオリアクティブシステム
- **マイルストーン**: MS5 Motion Graphics
- **関連要件**: REQ-MEDIA-003
- **規模**: M
- **依存タスク**: TASK-011, TASK-007

## 概要
AudioAnalysisノード、RMS/ピーク振幅抽出、FFTスペクトラム解析（rustfftクレート、低域/中域/高域バンド分離）、ビート検出・オンセット検出（MITライセンス代替）、自動BPM検出、タイムライン上のビートマーカー配置、キーフレームのBPMグリッドスナッピング、解析出力の統一アニメーションチャネル接続を実装する。

## 実装ステップ
1. AudioAnalysisノード実装
2. RMS/ピーク振幅抽出実装
3. FFTスペクトラム解析実装（rustfftクレート、低域/中域/高域バンド分離）
4. ビート検出・オンセット検出実装（aubioのMITライセンス代替を使用）
5. 自動BPM検出実装
6. タイムライン上のビートマーカー配置実装
7. キーフレームのBPMグリッドスナッピング実装
8. 解析出力を統一アニメーションチャネルへ接続

## 対象コンポーネント
- `crates/ravel-audio/src/analysis/` (オーディオ解析)
- `crates/ravel-audio/src/fft/` (FFTスペクトラム)
- `crates/ravel-audio/src/beat/` (ビート検出・BPM)
- `crates/ravel-core/src/nodes/audio_reactive/` (オーディオリアクティブノード)
- `crates/ravel-ui/src/timeline/beat_markers/` (ビートマーカーUI)

## 完了条件
- [ ] AudioAnalysisノードが動作する
- [ ] RMS/ピーク振幅が正しく抽出される
- [ ] FFTスペクトラム解析で低域/中域/高域バンド分離が動作する
- [ ] ビート検出・オンセット検出が動作する
- [ ] 自動BPM検出が正確に機能する
- [ ] タイムラインにビートマーカーが配置される
- [ ] キーフレームがBPMグリッドにスナッピングされる
- [ ] 解析出力がアニメーションチャネルに接続済み
