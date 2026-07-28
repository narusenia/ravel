# [HIGH-22] ポートのヒットテストが z 順を無視し、背面ノードのポートが前面ノードより優先される

| 項目 | 内容 |
| --- | --- |
| 深刻度 | high |
| 種別 | bug |
| 領域 | ravel-app / NodeEditor |
| 該当 | `crates/ravel-app/src/node_editor/painting.rs:790-824`, `:826-835`, `crates/ravel-app/src/panels/node_editor.rs:1319-1337`, `:1733-1741` |

## 現状

ノード本体とポートでヒットテストの走査順が食い違っている。

**ノード本体** — z 順で走査し、**最後の hit（最前面）を残す**。

```rust
// node_editor.rs:1326-1337
let mut hit = None;
for node in painting::z_ordered(graph) {
    // ...
    if lx >= sx && lx <= sx + w && ly >= sy && ly <= sy + h {
        hit = Some(node.id);
    }
}
```

**ポート** — 生の `graph.nodes()` を走査し、**最初の hit を即 return** する。
z を一切見ていない。

```rust
// painting.rs:791-808
for node in graph.nodes() {
    if node.metadata.synthetic { continue; }
    // ...
    for (i, _input) in node.inputs.iter().enumerate() {
        if dist <= PORT_HIT_RADIUS {
            return Some(PortHit { /* ... */ });   // ← 反復順が勝つ
        }
    }
```

さらに `on_mouse_down` はポート判定を**ノード本体判定より先**に実行する
（`node_editor.rs:1733-1741` の `port_at_local_pos` が
`node_at_local_pos` より前）。

結果、ノードが重なっている領域では**背面ノードのポートが前面ノードの本体より
優先される**。ユーザーには「前面のノードを掴んだつもりが、下のノードから
エッジが伸びる」と見える。しかも反復順は `im::HashMap` の順序なので、
どのノードが勝つかは編集履歴に依存して変わる。

`find_snap_target`（`painting.rs:826-835`）も同じく生の `graph.nodes()` を
走査するため、距離が等しい重なりでどちらに吸着するかが不定。

## 影響

ノードを重ねて配置したときに配線が意図しない相手に繋がる。エッジは
`add_edge` でポートの存在も型も検証されない（`crates/ravel-core/src/graph.rs:629`）
ため、間違った接続はエラーにならずそのまま成立する。

## 修正方針

- `port_at_local_pos` を `painting::z_ordered` で走査し、**最後の hit を残す**
  （または逆順で走査して最初の hit を返す）。ノード本体側と同じ規約に揃える
- `find_snap_target` は距離が主基準のままとし、**同距離の tie-break に z を使う**
- ヒットテストの優先順位（ポート → エッジ → ノード本体）をコメントで明示する。
  現状は分岐の順序だけが仕様になっている

## 検証

- 2 つのノードを重ね、前面ノードの本体上でマウスダウンしたとき背面ノードの
  ポートが選ばれないテスト
- ポートが重なっているとき前面ノードのポートが返るテスト
- 同距離に 2 つの吸着候補があるとき前面が選ばれるテスト
- 上記を `im::HashMap` の反復順に依存しない形で書く（ノードの追加順を
  変えても結果が同じであることを確認する）

## 関連

- [HIGH-21](HIGH-21-node-editor-repaints-every-playback-frame.md) — 同じパネルの
  再描画コスト問題
- `docs/implementation/viewer-overlay-manipulator-plan.md` 単位 1 —
  Viewer 側のオーバーレイ機構では同じ罠を避けるため、ハンドルのヒットテストを
  優先度宣言で解決する設計にしている
