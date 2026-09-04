# medium — ravel-gpu / ravel-nodes（GPU パイプライン・ノード処理）

---

## MED-GPU-06 | debt | リードバックのステージングプールが共有 `CacheBudget` の外にある

**該当**: `crates/ravel-gpu/src/staging.rs`（`IDLE_BUDGET_BYTES = 256 MiB`）、
`crates/ravel-gpu/src/device.rs`（`GpuContext` がプールを保持）

`GPUBK-6`（#282）が入れたステージングプールは、アイドル分の上限を**自前の
256 MiB 定数**で持つ。`CACHE-3` が立てた「メモリの権威を 1 つに」という原則
（`ravel_core::cache_budget::CacheBudget` が VRAM / RAM / Disk を一元管理する）
の外にある唯一の GPU 側プールで、**ユーザーが設定でメモリ上限を下げても
ここには効かない**。

そうなっている理由は妥当で、単に安易な移し替えができないというだけ:

- ステージングは `COPY_DST | MAP_READ` すなわちホスト可視メモリ。`Tier::Vram` に
  計上すると `TexturePool` が*デバイス*テクスチャを*ホスト*バッファのために
  evict する誤った取引になる
- `Tier::Ram` に載せるには `GpuContext` が `SharedCacheBudget` を保持する必要が
  あり、`GpuContext::from_handles`（アプリのデバイス共有経路）まで含む構築 API の
  変更になる

実害の大きさ: 定常的に保持されるのは解像度ごと 1 本なので、1080p 表示なら
約 32 MiB、4K なら約 127 MiB。上限に達するのは複数解像度が同時に動く場合だけ。
変更前は毎フレーム確保・解放していたので**総量は増えるが churn は消えている**。

**修正方針**: `GpuContext` に `SharedCacheBudget` を渡し、
`Tier::Ram` の headroom をアイドル許容量にする（`TexturePool::with_shared_budget`
が `Tier::Vram` に対してやっているのと同じ形）。ロック順は
プール → 予算を守る。`GPUBK-9`（デバイス共有の契約）が構築 API を触るので、
そこに相乗りするのが安い。

---

## MED-GPU-08 | bug / debt | `rasterize` がパスの点ごとの色を一切描かない（Point ドメインの `Cd` が無音で捨てられる）

**該当**: `crates/ravel-nodes/src/rasterize/mod.rs:503`（`element_colors`）、
`:607`（`path_vertex_mask`）、`:753`、`:885`

`rasterize` は描画要素の種類でドメインを決めて色を引く:

| 描くもの | 読むドメイン |
| --- | --- |
| パス（`shape.rect` / `shape.line` の塗り・線） | **Primitive** |
| どのプリミティブにも属さない点（スプライト） | Point |
| インスタンス | Instance |

`path_vertex_mask` が「プリミティブに覆われた点」をスプライト描画から外すので、
**パスの頂点に書いた `Cd` は 1 画素も描かれない**。`attribute.set` の `domain`
既定が `"point"`、`field.apply` の既定も点なので、**素直に組むと必ずここに落ちる**。
エラーも警告も出ず、絵が変わらないだけ。

**フェーズ D が掲げる完成形が画素まで届かない**:

```
shape.line → attribute.curveu → field.attribute("u") → field.ramp
  → field.apply(target = "Cd") → rasterize
```

`field.apply` までは正しく点ごとの色が乗る（`style-attributes-plan.md` 単位 6 の
テストが pin している）が、`rasterize` が読まない。

**修正の障壁**（着手前に読むこと）:

- **CPU と GPU の両方が要る。** `rasterize` は zeno（CPU）と WGSL（GPU）の
  2 本立てで、`RESP3-12` 以降**ゴールデンが「CPU と GPU が許容誤差内で一致
  すること」を検査する**（`crates/ravel-nodes/tests/shape_layer_golden.rs`）。
  片方だけ直すと一致が壊れる
- **zeno はカバレッジマスクしか返さない。** 現在の CPU 経路は
  `Mask::render_into` で 0..255 の被覆を得て 1 色でブレンドする
  （`Canvas::blend_coverage`）。頂点色の補間には「その画素がどの頂点の間にあるか」が
  要り、被覆マスクだけでは出せない。**CPU 側の方式決定が最初の関門**
- **`stroke_align` が同じ理由で `style-attributes-plan.md` 単位 3 へ繰り延べ
  済み。** CPU に概念が無いものを近似すると CPU/GPU 一致が壊れる、という同じ構図。
  **この 2 つは同じ判断の下にあるのでまとめて設計する価値がある**
- 複数クレートに跨るので **AGENTS.md の設計ゲートに該当する。計画書が要る**

**却下した案**（2026-08-13）:

1. Point ドメインの `Cd` をエラーにする — 「効かないのに成功する」は消えるが、
   やりたいこと（線に沿ったグラデーション）ができない
2. Point → Primitive のフォールバック（先頭 or 平均） — 期待と違う絵が出る
3. **頂点補間** — 採用方針。ただし上の障壁により計画書が要る

**関連**: `style-attributes-plan.md` 単位 6 の注記（完了条件をこの制約に
合わせて直してある）、同 単位 1 / 単位 3（`stroke_align`）

---

## MED-GPU-09 | bug | `Placement::compose` が非一様スケールと回転の合成で誤った変換を作る

**該当**: `crates/ravel-nodes/src/rasterize/mod.rs` の `compose()` と
`Placement`（`offset` / `rot` / `scale` の分解表現）

`rasterize` はインスタンスの配置を **回転角とスケール成分に分解した**
`Placement` で持ち、入れ子を `compose(outer, local)` で畳む。畳み方が

```
rot   = outer.rot + local.rot
scale = outer.scale * local.scale   （成分ごと）
```

なので、実際の合成 `R_o S_o R_l S_l` を `R_(o+l) S_o S_l = R_o R_l S_o S_l`
として扱っている。**これが一致するのは `S_o` が `R_l` と可換なとき**、
つまり**外側のスケールが一様**か**内側の回転が 0** のときだけである。

`offset` の合成（`outer.apply(local.offset)`）は正しい。ずれるのは
**線形部分だけ**。

**再現**（外側 `scale = (2, 1)`、内側 `rot = 90°`、点 `p = (1, 0)`）:

| 計算 | 結果 |
| --- | --- |
| 正しい `outer.apply(local.apply(p))` | `(0, 1)` |
| 現在の `compose(outer, local).apply(p)` | `(0, 2)` |

**影響範囲**:

- **ジオメトリのネストインスタンス**（`scatter` の中の `scatter` など）で、
  外側が非一様スケール・内側が回転を持つと形が崩れる
- **画像インスタンス**（`IMG-4`）も同じ `Placement` を通るので同じだけずれる。
  ただし `raster_image` の逆変換は `Placement::apply` の**厳密な逆**であり、
  画像経路とジオメトリ経路は互いに一貫している。**`IMG-4` が入れた退行では
  ない**
- 最上位の `Placement::for_context` はコンポ → キャンバスのスケールを持つ。
  これは `resolution / comp_resolution` の軸ごとの比なので、**丸めによって
  わずかに非一様になりうる**（例: 1920x1081 を Half で 960x540 に落とすと
  `(0.5, 0.4995…)`）。この場合、回転したインスタンスが微妙にずれる

**修正の方向**: `Placement` の線形部分を `rot` / `scale` の分解ではなく
**2×2 行列**で持ち、`compose` を行列積、`raster_image` の標本化を逆行列に
する。`offset` はそのまま。

**修正の障壁**（着手前に読むこと）:

- **GPU 経路にも波及する。** `flatten_geometry` が同じ `compose` を通り、
  `DrawItem` の `data0` / `data1` に配置を詰めて WGSL 側で使う。行列表現に
  変えるなら詰め方とシェーダの両方が変わる
- **`Placement::uniform_scale()` の利用側**（`dash_pattern` と
  `stroke_width` のスケーリング）が分解表現に依存している。行列からどう
  「代表スケール」を出すかを決める必要がある
- **既存ゴールデンが動く可能性がある。** 一様スケールのケースは数値的に
  同じはずだが、浮動小数の演算順序が変わるので許容誤差の確認が要る
- CPU と GPU の一致ゴールデン（`RESP3-12` 以降）が両側を同時に見るので、
  **片側だけ直すと一致が壊れる**

**関連**: `done/image-instancing-plan.md`（`IMG-4` / `IMG-5`）、`MED-GPU-08`

**2026-09-04 に `TYPE-5`（PR #511）の実装で独立に再発見された。** そちらは
配置の式を `InstanceTransform` としてコアに集約し、`rasterize::Placement` を
そこへ委譲したので、**この近似は今 1 箇所（`InstanceTransform::compose`）に
閉じている** — 直す場所が 1 つになった。型の doc にも天井として明記済み。

## MED-GPU-10 | bug | `geometry.transform` がベジェ接線を変換しないので、回転・スケールで曲線の形が壊れる

**該当**: `crates/ravel-nodes/src/geometry.rs`（`GeometryTransformProcessor`）

`geometry.transform` が書き換えるのは Point ドメインの `P`、Detail の
`anchor`、Instance の `P` / `rot` / `scale` だけで、**`in_tan` / `out_tan` を
触らない**。パスの制御点は「点からのオフセット」なので、点だけ回して接線を
置き去りにすると**曲線の形が変わる** — 90 度回した円が卵になり、
拡大した曲線は制御点だけ元の長さのまま残る。

**これはテキスト以前からあるバグ**である。`shape.custom_path`
（`done/viewer-tool-extensions-plan.md` の `TOOLX-3`、ペンツールの出力）が
接線を持つので、**ペンで描いた曲線を `geometry.transform` に通すと壊れる**。
`text.layout`（`TYPE-2`）の輪郭も接線を持つようになったので、踏みやすくなった。

**直し方は既にリポジトリの中にある**: `TYPE-5`（PR #511）が
`InstanceTransform::apply_vector` を入れた — 「差分は回転とスケールだけ、
平行移動は掛けない」という変換で、接線に必要なのはまさにこれである。
`geometry.transform` の `apply` の隣に同じ線形部分を持たせ、
`in_tan` / `out_tan` の列があれば通す。

**severity の根拠**: bug。データは壊れないが**出る絵が間違う**。high でないのは
接線を持つジオメトリを `geometry.transform` に通したときだけで、
既定のシェイプ（rect / ellipse / polygon / star）は接線を持たないため。
low でないのは、ペンツールで描いた曲線という**ユーザーが直接作ったもの**が
黙って変形するため。

**関連**: `MED-GPU-09`（`Placement::compose` のアフィン近似）は
`TYPE-5` の実装で**独立に再発見された** — 同じ穴に別の入口から 2 回当たって
いるので、`InstanceTransform` に寄せた今が直しやすい。

