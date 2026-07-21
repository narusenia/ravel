# Viewer コンプ座標スケール修正計画

## 問題

対話 Viewer は評価解像度を長辺 1024px にキャップする
（`crates/ravel-app/src/project_state.rs` の `VIEWER_MAX_DIM` /
`viewer_resolution`。殻コンポジットが CPU + GPU readback のためフル解像度で
フレームレートが持たない暫定策）。一方でジオメトリ座標はコンポジション空間
（フルコンプ解像度のピクセル値）で作成され、rasterize はそれを無スケールで
出力フレームへ 1:1 描画する。結果:

- コンプ中心 `(960, 540)` に置いたシェイプが 1024×576 キャンバスの
  右下 93.75% 位置に描かれる（スモークテストで顕在化）。
- すべてのジオメトリが枠に対して `comp / capped`（1920×1080 なら 1.875）倍
  大きく表示される。
- さらに深い不整合として、レイヤー網境界 `CompNetworkProcessor`
  （`crates/ravel-nodes/src/comp/mod.rs`）と `LayerRefProcessor`
  （`crates/ravel-nodes/src/layer_ref.rs`）が内側ネットワークの
  `ctx.resolution` を**フルコンプ解像度へ書き換える**ため、レイヤー網の
  rasterize はコンプ解像度でフレームを生成し、キャップ解像度で動く外側の
  殻チェーン（`comp.transform` / `comp.merge`）がそれを**左上 crop** する。
  Viewer 表示（`ViewerViewport::rect` はフルコンプ解像度基準の contain fit）
  がその crop 済みバッファを引き伸ばすため、「左上を 1.875 倍ズームした
  クロップ」が表示されていた。

座標系の定義が「コンプ空間」と「出力キャンバス空間」で混線していることが
根本原因。レンダリング品質のフル解像度出力パスは未実装
（`ravel-media` に評価パスなし）なので、現時点の唯一の評価者である Viewer
を正しくすることが目的。

## 目標アーキテクチャ

**ジオメトリ座標・レイヤー殻 transform の値はコンポジション空間**とし、
**ピクセルを生成するノードだけが「コンプ空間 → キャンバス」のスケールを
適用する**。

- `EvalContext`（`crates/ravel-core/src/eval.rs`）に
  `comp_resolution: (u32, u32)` を追加する。意味論:
  `resolution` = 出力キャンバスのピクセルサイズ、`comp_resolution` =
  座標系の基準となる所属コンポジションの解像度。スケール係数は
  `(resolution.0 / comp_resolution.0, resolution.1 / comp_resolution.1)`
  （`viewer_resolution` はアスペクト保存なので実質等方。丸め誤差は
  軸別係数で吸収する）。
  - `EvalContext::new(frame, fps, resolution)` は
    `comp_resolution = resolution`（scale 1）で初期化し、既存テスト・
    ゴールデン・ベンチを無変更で通す。Viewer 等は builder
    （`with_comp_resolution`）で設定する。
  - `EvalContext` は `PartialEq` でキャッシュ鍵に参加するため、新フィールドは
    自動的にキャッシュ正当性へ反映される（`CacheMiss::ResolutionChanged`）。
- **発行元**: `ProjectState::build_viewer_request`
  （`crates/ravel-app/src/project_state.rs`）が
  `resolution = viewer_resolution(comp.resolution)`、
  `comp_resolution = comp.resolution` を設定する。
- **境界の書き換えを廃止**: `CompNetworkProcessor` と `LayerRefProcessor` の
  ctx 構築は `resolution` を外側の値のまま維持し、`comp_resolution` を
  所属コンプの解像度に設定する。layer.ref が参照する側のコンプ解像度が
  異なる場合は、外側の等方スケール `s = 外側 resolution / 外側 comp_resolution`
  を保ち `resolution = round(参照先 comp_resolution × s)` とする
  （コンプごとの WYSIWYG を維持）。
- **rasterize**（`crates/ravel-nodes/src/rasterize/mod.rs`、CPU / GPU 両経路）:
  トップレベル `Placement` を `identity()` から
  `scale = resolution / comp_resolution` の Placement に変更する。既存の
  `Placement::apply` / `compose` / `uniform_scale()` を通じて、頂点位置・
  インスタンス配置・stroke 幅・pscale（ポイントスプライト半径）が一括で
  スケールされる（等方スケールなので `uniform_scale` は正確）。
  シェーダ uniform の `resolution`（ピクセル→NDC 変換）はキャンバス値のままで
  変更しない。
- **base_quad**（`crates/ravel-nodes/src/net.rs`）: レイヤーの基準クアッドは
  コンプ空間 `(0,0)..(comp_resolution)` で生成する（rasterize が縮める）。
- **comp.transform**（`crates/ravel-nodes/src/comp/transform.rs`）:
  殻 transform の position / anchor チャンネル値はコンプ空間ピクセルなので、
  行列構築時に平行移動成分へ同スケールを適用する（scale / rotation は無次元で
  変更なし）。
- **comp.merge**: 内側と外側の resolution が一致するようになるため、
  pad / crop 正規化は本来の役割（異サイズ入力の保護）に戻る。変更なし。
- **GpuEvalHooks の geometry フォールバック rasterize**
  （`crates/ravel-app/src/eval_hooks.rs`）: ctx をそのまま流用しているため、
  rasterize 側の対応で自動的に正しくなる（明示変更なし。検証のみ）。

### スコープ外

- フル解像度レンダリング出力パス（GPU コンポジット / zero-copy Viewer と
  同時に別計画で実施）。
- `VIEWER_MAX_DIM` の値の見直し。
- フィールド系ノードの座標系監査で問題が出た場合の個別修正
  （ctx.resolution を読むノードは現状 rasterize / comp.* / net.rs のみと
  調査済みだが、実装中に追加発見があれば本計画に追記する）。

## 実装単位

1. **`EvalContext.comp_resolution` 基盤**（ravel-core）:
   フィールド追加、`new` の既定 scale 1、`with_comp_resolution`、
   キャッシュミス種別の挙動確認。構造体リテラル構築箇所
   （`comp/mod.rs` / `layer_ref.rs`）のコンパイルエラー解消を含む。
2. **境界とスケール適用**（ravel-nodes）: `CompNetworkProcessor` /
   `LayerRefProcessor` の ctx 構築変更、rasterize トップレベル Placement、
   base_quad、`comp.transform` の平行移動スケール。
3. **発行元と検証**（ravel-app）: `build_viewer_request` の設定、
   `eval_hooks` フォールバックの検証、実機確認。

1 と 2+3 は分離した PR にできるが、2 は 1 に依存する。

## 完了条件

- 1920×1080 コンプで `(960, 540)` 中心のシェイプが Viewer の中心に表示される
  （キャップ解像度評価のまま）。
- シェイプの大きさが枠に対して正しい比率で表示される（100px 矩形が
  コンプ幅の 100/1920 を占める）。
- レイヤー殻 transform の position 移動量が Viewer 上で見た目どおりに効く。
- 既存ゴールデン・既存テスト（ctx 構築が `EvalContext::new` のもの）は
  scale 1 のまま無変更で green。
- 新テスト: `resolution ≠ comp_resolution` の ctx で矩形の描画位置・サイズ・
  stroke 幅・pscale がスケールされることを CPU リファレンスで検証し、
  GPU/CPU 等価性テストにも同条件を追加する。
- レイヤー網境界を跨ぐ評価（レイヤー付きコンプ）で crop が発生しない。
- `EvalContext` は直列化されず（`Serialize` 派生なし）、プロジェクト形式・
  ジャーナルへの影響はない。
