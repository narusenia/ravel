# HIGH-27 | bug | Subnet に畳んだノードのキーフレームが Timeline から消えるのに、アニメーションは続く

`crates/ravel-ui/src/keyframes.rs:150`（`property_rows`）、`:184`（`row_channels`）、
`:748`（`mutate_channel`）

## 症状

ノードを選択して Collapse to Subnet（`NETIF-6`、PR #304）すると、そのノードの
パラメータに打ってあったキーフレームが **Timeline のプロパティツリーから消える**。
一方で評価器はそのキーフレームを読み続けるので、**アニメーションは動いたまま**。
ユーザーからは「キーフレームが無いのに動いている」ように見え、止めることも
編集することもできない。

## 原因

Timeline のプロパティ行の列挙・解決・変更が、いずれも**レイヤーネットワークの
最上位グラフだけを平坦に見ている**。Subnet の内部グラフへ降りない。

```rust
// keyframes.rs:150 — property_rows
let mut nodes: Vec<_> = layer.network.nodes().collect();
```

```rust
// keyframes.rs:184 — row_channels
let node_ref = layer.network.node(*node)?;
```

```rust
// keyframes.rs:748 — mutate_channel
let Some(node_ref) = layer.network.node(*node) else { return false };
// …
layer.network = layer.network.clone().replace_node(Arc::new(updated));
```

`Graph::node` は `self.nodes.get(&id)`（`graph.rs:719`）で、`node.subnet` の
内部グラフを一切見ない。`collapse_to_subnet`（`network.rs:1536`）は選択ノードを
内部グラフへ移すので、移った時点で 3 つの関数すべてから見えなくなる。

評価側は逆に、ネットワーク境界ノードを通って**再帰的に**評価する（AGENTS.md の
「layer networks are evaluated recursively through the network boundary node」）。
**列挙が平坦で評価が再帰的**という非対称が、そのまま症状になっている。

`keyframes.rs:10-13` のモジュールコメントは「subnet-promoted parameters
(both are plain node parameters of the layer network)」と書いており、
**promote された側**は最上位に出るので見える。畳まれた**内部ノードそのもの**が
持つキーフレームは対象外のまま — 仕様として意図されたのか、単に見落としたのかが
コメントからは読めない。

## 修正方針

`property_rows` / `row_channels` / `mutate_channel` を内部グラフへ再帰させる。
`NodeId` は単一のグローバルカウンタ（`id.rs:11` の `NODE_ID_COUNTER`）から採番
されるので**内部グラフを含めて一意**であり、`PropertyRowId::Network { node, key }`
を**パス化しなくても**アドレスとしては足りる。よって:

1. 列挙: `layer.network` を再帰的に走査する。行ラベルは Subnet 名で修飾しないと
   同名パラメータが並んで区別が付かない（`Blur · radius` に対し
   `Subnet 1 / Blur · radius` のような形）
2. 書き戻し: `replace_node` は最上位にしか効かないので、**どの内部グラフの
   ノードか**を辿って `node.subnet` を差し替える経路が要る。ここが実質の作業
3. 順序: 現在の「node id 順」は再帰後も決定的である必要がある

### 注意 — `MED-APP-25` との相互作用

`MED-APP-25`（Subnet のコピー＆ペーストが内部グラフの `NodeId` を複製しない）が
残っている間は、**内部 `NodeId` が一意である前提が崩れる**。その状態で行を
bare `NodeId` でアドレスすると、コピーされた 2 つの Subnet のどちらの行なのかが
決まらない。**`MED-APP-25` を先に直すか、同時に直す。**

## 未確認

- 同じ操作で「ワイヤの接続がおかしくなる」という報告も出ている。こちらは
  `collapse_to_subnet` の再配線側の話で、本項とは別の機構。**未調査・別項目**。
- Extract（Subnet の展開）で対称の問題が起きるかは未確認。展開は内部ノードを
  最上位へ戻すので、行は復活する見込みだが、確認していない。
