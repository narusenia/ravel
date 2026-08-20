# [HIGH-35] 参照 ID のパラメータをワイヤで動かせる — フレームごとに参照先が変わり、ID の予約が外れる

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-core / graph・eval, ravel-app / NodeEditor |
| 該当 | `crates/ravel-core/src/graph.rs`（`expose_param_port`）, `crates/ravel-core/src/eval.rs`（`param_port_overlay`）, `crates/ravel-core/src/composition/mod.rs`（`id_watermarks`） |

## 現状

**参照先の生の ID を持つパラメータが、パラメータポートとして公開できる。**

該当するのは `composition::validate::is_identifier_parameter` が列挙する 3 つ:

- `layer.ref` の `layer`（`LayerId` の生値）
- `precomp` の `comp_id`（`CompId` の生値）
- `media` の `asset_id`（`AssetId` の 10 進表記）

`Graph::expose_param_port` はこれらを**普通の `SCALAR` 入力として受ける**。
Scalar を繋ぐと `param_port_overlay` が `ResolvedValue::Int` に変換するので、
**評価のたびに参照先 ID が変わる**。

> **訂正（`render-warning-channel-plan.md` の着手時に実測）。**
> ワイヤで動くのは `layer.ref` の `layer` と `precomp` の `comp_id` の 2 つだけ。
> `media` の `asset_id` は `ParameterValue::String` で、
> `param_port_overlay` は `String` / `StringSteps` に `None` を返すので
> **ワイヤでは動かない**。ただし `asset_id` には**同じ状態に至る別の口**がある:
> `DISK-2` が足した `StringSteps` を手編集で置くと、`eval.rs` が
> フレームごとに `sample` して `media` の `str_or("asset_id", "")` に渡す一方、
> `composition::node_asset_reference` は `ParameterValue::String` だけを読むので
> `id_watermarks` がその ID を予約しない。**欠陥の形と帰結は同じ。**

一方 `Document::id_watermarks` は**保存された値**しか走査しない
（`ParameterValue::static_identifier` 経由）。したがって:

1. ワイヤで駆動される参照先の ID は**予約されない**
2. 新しく採番した `LayerId` / `CompId` / `AssetId` が**その番号を引き当てる**
3. **参照が無関係なレイヤー / コンプ / 素材に繋がり直す**（REQ-LAYER-009 違反）

`.ravprj` v9（`AID-1` / `AID-2`）が素材について消したのと**同じ形の欠陥**が、
パラメータポート経由で残っている。

## 影響

到達には (a) `Graph::expose_param_port` を API から呼ぶ、(b) 既にそのポートを
持つ文書を開く、のどちらかが要る。**UI 経路は `DISK-3`（#462）で塞いだ**
（`toggle_param_port` と Properties の `port_states` が識別子を拒否する）ので、
今日の GUI 操作からは到達しない。

ただし塞いだのは**公開する側だけ**で、既に公開済みのポートは解除できるように
残してある（解除手段を消すと詰むため）。つまり**手編集した `.ravprj` や
古い文書は依然としてこの状態を持てる**。

## 修正方針

**決着済み**: 案 3（評価時に無視して警告する）を
`docs/implementation/render-warning-channel-plan.md` の `WARN-1` / `WARN-2` が
実施する（`HIGH-34` と同じ警告経路を共有する）。以下は判断の記録。

**決めが要る。** 3 つの案があり、それぞれ代償が違う。

1. **`Graph::expose_param_port` で拒否する。** 一番早い。ただし
   **既存文書にそのポートがあると読み込み後に整合しない**（グラフは
   `from_parts` で再構築されるので、拒否すると開けなくなる可能性がある）
2. **`Document::validate` で拒否する。** 明確だが、**保存できたものが
   開けなくなる**方向（`HIGH-26` の教訓に反する。既存文書を壊す）
3. **評価時に無視する。** `param_port_overlay` が識別子パラメータのとき
   ワイヤを無視して保存値を使い、一度だけ警告する。**既存文書は開ける**まま
   参照が動かなくなる。`HIGH-34`（オフラインの無言）と同じ「警告経路」の問題を
   共有するので、そちらと一緒に設計するのが素直

**案 3 を推す**が、警告をどこに出すか（`tracing` だけでは足りない）が
`HIGH-34` と同じ未解決なので、2 件をまとめて扱うのが良い。

## 備考

`DISK-2` / `DISK-3`（#462）の独立レビューで発見した。**この欠陥は
`DISK-*` が持ち込んだものではない** — パラメータポート（`param-input-ports-plan`）と
識別子パラメータはどちらも以前からあり、`DISK-3` が
「識別子はアニメートさせない」という規約を初めて明文化したことで、
**同じ規約がポート経路では守られていない**ことが見えた。
