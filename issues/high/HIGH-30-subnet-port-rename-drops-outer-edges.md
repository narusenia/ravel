# HIGH-30 | bug | Subnet の中でポート名を変えると、外側のエッジと promoted パラメータのキーフレームが黙って消える

`crates/ravel-core/src/network.rs:1231-1245`（`sync_subnet_pins` の名前照合）、
`crates/ravel-ui/src/document.rs:766-770`（内側コミットのたびに発火）、
`crates/ravel-core/src/graph.rs:1682-1683`（エッジ削除）、
`crates/ravel-core/src/network.rs:1163-1177`（`promote_parameters` の再シード）

## 症状

Collapse to Subnet で畳んだあと、**中に入って `net.in` / `net.out` の
カスタムポートをリネームすると、外側の subnet ノードに繋がっていたエッジが
消える。** 新しい名前のピンは末尾に無配線で現れる。

同時に失われるもの:

- 外側 subnet ノードの **promoted パラメータの値とキーフレーム**
- 出力側をリネームした場合、そのピンを参照していた
  **`ChannelSource::NodeOutput` バインドが `Constant` に潰れる**
  （キーフレームもブレンドも消える。`graph.rs:1600` の doc が
  「irreversible losses」と認めている）

警告も UI 上の痕跡も無い。

## 原因

**エッジはポートを名前ではなくインデックスで指している**
（`graph.rs:620-626` の `Edge { source_port: OutputPortIndex,
target_port: InputPortIndex }`）。だから `Graph::rename_port`
（`graph.rs:1422-1481`）はエッジを一切触らない。doc も
「Edges address ports by index, so no edge moves」と書いてあり、
**単一グラフ内で完結するリネームでは何も壊れない**。

壊れるのは**サブネット境界**。

`subnet_pins`（`network.rs:1090-1117`）は外側ピンを内側 In/Out の
**名前**から導出する。`sync_subnet_pins`（`:1231-1245`）はその宣言と
現在のピンを**名前で照合**する:

```rust
// 旧名のピンが新しい宣言に無い → ポートごと削除（＝外側エッジ削除）
if !inputs.iter().any(|p| p.name == port.name) {
    graph = graph.remove_input_port(subnet_id, InputPortIndex(index as u32))?;
}
```

**リネームという情報がここまで届いていない**ので、同期層からは
「削除 + 追加」にしか見えない。`remove_input_port_and_reindex`
（`graph.rs:1682-1683`）が `removals.push(edge.id)` で外側エッジを消す。

`rebuild_subnets`（`document.rs:753-772`）は `replace_network` 経由の
**すべての**内側グラフ書き込みでこの同期を走らせるので、Properties の
Ports セクションからでもノードエディタの「ポート名を変更」からでも同じ。

**同型の伝播路は既にある。** `PortEdit` / `KeyRename`
（`network.rs:713-748`）が露出パラメータ宣言へのリネーム伝播のために
作られている。**囲うサブネットノードのピン同期にだけ無い。**

## 条件

- **サブネット内でのみ**起きる。レイヤールートのネットワークでは起きない
- **`net.in` / `net.out` のカスタムポートのみ**（fixed ポートはリネームが
  拒否される）
- 型に依存しない
- 失われるのは**外側のエッジだけ**。内側のエッジは無傷
- Undo は 1 ステップで戻る（`edit_custom_ports` → `commit_document` が
  単一スナップショット）。ただし気づかずに作業を続けると復旧できない

## 再現

1. ノードを 2 つ以上選んで Collapse to Subnet（`NETIF-6`）
2. subnet ノードの中へ入り、`net.in` を選ぶ
3. Properties の Ports セクションでカスタム出力ポートをリネーム
4. 一階層上がる → **外側のエッジが消えている**

出力側も同じ（`net.out` のカスタム入力ポートをリネーム）。

## なぜ気づきにくいか

**`HIGH-27` と症状が重なる。** サブネット内のキーフレームは Timeline の
プロパティツリーに元々出ていないので、promoted パラメータのキーフレームが
消えても**消えたことが見えない**。

## 影響

`NETIF-6`（Collapse to Subnet）が入って以降、**通常のワークフロー
（畳む → 中で名前を整える）で踏む**。ユーザーの意図（名前を変えたい）と
結果（配線とアニメーションが消える）が完全に乖離している。

## 修正方針

`PortEdit` / `KeyRename` と**同型の仕組みを 1 段上げる**。
`rename_custom_port` が返す「旧名 → 新名」を `rebuild_subnets` が
`sync_subnet_pins` へ渡せる形にし、**名前照合の前に外側ピンをリネームする**。

**リネームの伝播路を 2 本作らないこと**（`MED-APP-25` の「採番規則を
2 つ置かない」と同じ理由）。

ロード時（`ravel-project/src/lib.rs:299-300`）にも同じ同期が走るので、
**削除が起きたときは `tracing::warn!` を出す**こと。保存済みピンが何らかの
理由でずれた `.ravprj` を開くと、ユーザー操作なしに外側エッジが消える。

### 回帰テスト

`ravel-ui` のヘッドレステストで書ける。

1. サブネット内 In のカスタム出力をリネーム → 外側入力エッジが生存し、
   新しい名前のピンに繋がっている
2. サブネット内 Out のカスタム入力をリネーム → 外側出力エッジと
   `NodeOutput` バインドが生存
3. 外側 subnet ノードの promoted パラメータにキーフレームを打ってから
   リネーム → キーフレームが新しいキーで生存
4. 入れ子サブネット（2 段）でも同じ

## 関連

- `HIGH-27` — サブネット内キーフレームの不可視。**この欠陥の発見を遅らせる**
- `MED-APP-25` — Subnet のコピペが内部 `NodeId` を複製しない。別問題
  （ID 採番であってポート名照合ではない）
- Extract Subnet（`network.rs:1878-1890`）はピン名で照合して見つからなければ
  黙って落とすので、この欠陥でピンがずれた状態だと配線を失う
