# ネットワークインターフェース編集 実装計画

> **Status**: 単位 1〜3 実装済み — 2026-08-02（2026-07-29 の計画から）

対象: In / Out ノードのカスタムポートを編集する手段と、Subnet ノードの
生成・整合。関連要件: REQ-LAYER-002、REQ-LAYER-003。

## 問題

評価側は完成しているのに、**編集手段が存在しない**。

`net.in` はカスタム出力ポートを評価でき（`crates/ravel-nodes/src/net.rs:44-84`。
binding 優先 → 同名パラメータ → 型ゼロ）、`net.out` はカスタム入力ポートを
`PortRecord` に集め（`:115-127`）、`subnet` は外側ピンを内部 In に名前で束縛して
再帰評価する（`crates/ravel-nodes/src/subnet.rs:56-106`）。入れ子の深さも
無制限でテスト済み。

だが**ポートを生やす経路が無い**。

### 1. ポートの追加・削除・改名・並び替えの API が無い

`Node::with_input` / `with_output`（`crates/ravel-core/src/graph.rs:314,325`）は
ビルダーのみ。既存ノードに生やすには `replace_node` を手書きするしかなく、
エッジの port index を自分で直す責任が呼び出し側に漏れる。

結果として、**カスタムポートはテストフィクスチャと `crates/ravel-app/src/project/mod.rs`
のデモデータにしか存在できない**。

### 2. 出力ポート側の再インデックス機構が存在しない（単位 1 で解消）

入力側には揃っていた（いずれも `crates/ravel-core/src/graph.rs`）。

| 関数 | 役割 |
|---|---|
| `remove_input_port_and_reindex` | 削除 + `Edge::target_port` の remap（private） |
| `insert_input_port_and_reindex` | 挿入 + 後続 index の押し出し（private） |
| `normalize_variadic_input_group` | 並び替え + remap の実例 |

`net.in` のカスタムポートは**出力**であり、`Edge::source_port` を remap する
対応物が無かった。単位 1 で `Graph::remove_output_port` /
`insert_output_port` / `rename_port` / `reorder_ports` として入り、
**`Edge::source_port` だけでなく `ChannelSource::NodeOutput` の
パラメータバインディングも同じ写像で移す**（出力ポートは 2 箇所から
index 参照されているため。詳細は `docs/agent-api-reference.md`）。

しかも `add_edge` は**ポートの存在も型も検証しない**
（型フィルタは UI 側のスナップだけ）。評価側は `inputs.get(i)` が `None` なら
未接続として扱うため、port index がずれたエッジは**エラーにならず黙って死ぬ**。
再インデックスを機構として持たない限り、この静かな破壊が必ず起きる。

### 3. Subnet ノードは Add Node から作ると壊れている

`NodeTemplate::create_node`（`crates/ravel-core/src/registry/mod.rs:152-167`）は
`subnet` フィールドを一切設定しない。`builtin.rs:334` のテンプレートは
ポートも内部グラフも空。よってコンテキストメニューから Subnet を追加すると
`subnet: None` のノードができ、評価すると
`subnet: node N has no inner graph` で失敗する。

内部 `net.in` / `net.out` の自動生成も、ノード群をサブネットにまとめる
操作（collapse）も無い。REQ-LAYER-003 の受入条件
「ノード群をサブネットワークにまとめられる」は未達。

### 4. 未接続のサブネット入力ピンが型を偽る（単位 2 で解消）

`NetInProcessor` は binding が無いカスタムポートを `custom_param_value` に
落としていた。この関数はスカラー / ベクトル / 色しか扱わず、それ以外は
`Scalar(0.0)` を返していた。

サブネットの内部 In が GEOMETRY ポートを持ち、外側ピンが未接続で同名
パラメータも無い場合、**Geometry を期待している下流に `Scalar` が流れる**。

単位 2 で、フォールバックはポート自身の型付きゼロ（`zero_value`）になった。
`FIELD` のゼロを作るために `ConstantField` を production へ昇格している
（フィールドはサンプラなのでゼロもサンプラでなければならない）。同じ経路を
使う `net.out` の未接続入力と `layer_ref` の区間外ゼロにも効く。

## 決定事項

### ポートは実体として持ち、Subnet は同期関数で内部から再生成する

`node.inputs` / `node.outputs` は描画・ヒットテスト・エッジ検証・
`PortRecord` の順序・永続化のすべてが読む。導出ビューに変えると影響範囲が
全域に及ぶため、**実体を持つ**。

Subnet ノードのピンは内部 In / Out が唯一の情報源だが、実体としては
マテリアライズし、`sync_subnet_pins(graph, subnet_id)` で再生成する。
呼ぶのは内部グラフ編集のコミット後とロード時（ドリフト修復）。
名前で対応付けて remap し、消えたポートのエッジを削除する処理は
`normalize_variadic_input_group`（`graph.rs:807-881`）と同型になる。

### In のカスタムポートに許す型は文脈で分ける

| 文脈 | 許す型 | 根拠 |
|---|---|---|
| レイヤールートの In | Float / Int / Bool / Vec2 / Vec3 / Color | 殻が供給できるのは値だけ（REQ-LAYER-002） |
| サブネット内の In | 上記 + Geometry / Field / FrameBuffer / Text | 内部 In はサブネットの入力ピン境界（REQ-LAYER-003） |

型 Select の選択肢は `NetworkPath` の subnet セグメントが空かどうかで切る。

未接続フォールバックは**型で分岐する**。パラメータ型は同名パラメータの値、
それ以外は `zero_value(port.data_type)`（`net.rs:177`）。これが問題 4 の修正。

### `f` の自動追加はレイヤールートの In のみ

`network.rs:33` の `PORT_FRAME_INDEX` はレイヤーローカルのフレーム番号で、
サブネット内 In のポートはサブネットのピン境界なので自動追加しない
（REQ-LAYER-002 受入条件の但し書き）。既存の legacy 衝突規則
（同名パラメータを持つ `f` はカスタム扱いを維持。`net.rs:52-59`、
`done/node-expansion-plan.md`）は変更しない。

### 編集 UI は Properties 主・ノードエディタ従

追加・型変更・並び替えは Properties パネルの Ports セクション。
単体の削除・改名はノードエディタのポート右クリック。両者は同じ graph API を
呼ぶので判定ロジックは 1 本。

Properties から graph を触る経路は既存の
`port_toggle_button`（`crates/ravel-app/src/panels/properties.rs:435-466`）—
`NodeEditorHandle` 経由で `NodeEditorPanel` のメソッドを呼ぶ — を踏襲する。

### 改名は 4 箇所を 1 つの操作で書き換える

In のカスタムポート名は、ポート名・同名パラメータのキー・Properties の
`custom.<name>` フィールド名・サブネットの promote 名を兼ねている。改名は
これらを 1 つの Document コミットで一括更新する。部分適用を作らない。

## 実装単位

### 単位 1: 出力ポートの再インデックス API — 実装済み（#258）

- `remove_output_port_and_reindex` / `insert_output_port_and_reindex` を
  入力側と対称に追加。`Edge::source_port` を remap し、消えるポートのエッジを
  削除する。公開経路は `Graph::remove_output_port` / `insert_output_port`
- `rename_port`（入出力共通。エッジは index 参照なので remap 不要、
  パラメータキーの追従が本体）
- `reorder_ports`（名前 → 新 index の写像から remap）
- いずれも `Graph` を返す既存の不変更新スタイル。1 呼び出し = 1 貫状態

**完了条件**

- ポート削除で、そのポートのエッジが消え、後続ポートのエッジ index が
  1 つ下がるテスト
- ポート並び替えで、全エッジの接続関係（source/target のノードとポート名の組）が
  保存されるテスト
- 出力・入力の両方で上記が成立するテスト

**実装で判明したこと**

- 出力ポートは **2 箇所**から index 参照される。`Edge::source_port` と
  `ChannelSource::NodeOutput(NodeId, OutputPortIndex)`（`Node::parameter_sources`）。
  エッジだけ直すとバインディングが 1 スロットずれた別ポートを黙って指す。
  消えたポートのバインディングは `ChannelSource::Constant` に潰す
  （エッジ削除と対になる不可逆な損失。undo 単位は呼び出し側の Document コミット）
- 「ポート名 == パラメータキー」は普遍則ではない。pairing を作るのは
  `is_param` の入力ポートと `net.in` の出力ポートの 2 機構だけで、
  `constant` の出力 `value` ⟷ パラメータ `value` のような**偶然の同名**が実在する
  （プロセッサは literal key で引くので巻き込んで改名すると壊れる）
- `ChannelSource::NodeOutput` は `Layer` の殻チャンネル（`transform` /
  `opacity` / audio gain）にも載り `.ravprj` に永続化されるが、`Graph` からは
  見えないのでこの API は追従しない。殻チャンネルは
  `AnimationChannel::evaluate` のグラフ非依存経路で読まれ `NodeOutput` は
  プレースホルダなので現状は無害。**グラフ文脈での解決を入れる際に
  Document 層の追従が要る**
- 固定ポートの保護は入っていない（単位 2 の担当）。**単位 2 が入るまで
  この API を UI に直結させない**

### 単位 2: In / Out のカスタムポート編集 API — 実装済み（#260）

- `add_custom_port` / `remove_custom_port` / `rename_custom_port`。In は
  出力 + 同名パラメータ、Out は入力を対象にする
- 許可型の文脈依存判定（レイヤールート / サブネット内）をコア側の関数として持つ
- 未接続フォールバックを型で分岐させる（`custom_param_value` →
  パラメータ型以外は `zero_value`）
- `Out` の `frame` ポートは削除・改名不可（殻の合成チェーンが消費する唯一の
  ポート）。`In` の `base_geometry` / `t` / `f` / `source` も同様

**完了条件**

- レイヤールートの In に Geometry ポートを作れないテスト
- サブネット内の In に Geometry ポートを作れるテスト
- 未接続の Geometry ピンが `Scalar` ではなく空 `Geometry` を返す回帰テスト
- 固定ポートの削除・改名が `Err` になるテスト

**実装で判明したこと**

- 許可型は `DataTypeId` では表せない。`Float` / `Int` / `Bool` は wire では
  どれも `SCALAR` だが、パラメータ種も Properties のウィジェットも別。
  ユーザーが選ぶのは「wire 型 + パラメータ種」の組なので、`CustomPortType`
  という別の列挙にした。単位 3 の型 Select はこれをそのまま列挙できる
- 「予約名の問い」と「保護の問い」は分ける必要がある。ずれるのは `f` だけで、
  同名パラメータを持つ legacy `f` は削除・改名できなければならない（さもないと
  駆動も削除もできないポートになる）。一方で新規作成・改名先としての `f` は
  常に禁止
- **legacy `f` を外したらその場で builtin `f` を戻す**。補うのは
  `Document::normalize_net_in_ports` だけで、これはロード時にしか走らない。
  戻さないと、そのレイヤーは次のロードまでフレーム番号を取れず、
  ロードで黙って復活する。サブネット内では戻さない（下の決定事項どおり）
- 名前とパラメータキーの一対一は無条件に守る必要がある。pairing が成立し
  **得る**ポートでは名前がそのままキーなので、パラメータを連れていなくても
  占有済みキーへの着地は衝突になる（`net.in` の GEOMETRY 出力を既存の
  スカラーパラメータ名へ改名すると、そのポートがパラメータを読み始める）
- **ポートの型変更 API はまだ無い。** 単位 3 の完了条件が「追加 → 型変更 →
  並び替え → 削除が 1 操作 1 undo」なので、`set_custom_port_type` は
  単位 3 で足す

### 単位 3: Ports セクション（Properties）

- `ravel-ui` に `PropertyField::PortList` を追加（行 = 名前・型・固定フラグ、
  末尾に追加行）。セクション生成は headless なので `ravel-ui` のテストで覆う
- `ravel-app` 側で行の描画（名前 Input、型 Select、削除ボタン、並び替えハンドル）と
  `NodeEditorHandle` 経由の graph 更新
- 固定ポートは読み取り専用で表示する（存在を隠さない）

**完了条件**

- In / Out 選択時に Ports セクションが出て、固定ポートとカスタムポートが
  区別して並ぶ `ravel-ui` テスト
- 追加 → 型変更 → 並び替え → 削除が 1 操作 1 undo になるテスト

**実装で判明したこと**

- **型変更 API が単位 2 に無かった**。`set_custom_port_type` として
  `network.rs` に追加した。ポートの index は動かさず、In なら
  `data_type` + 同名パラメータ、Out なら `accepted_types` を張り替える。
  エッジは**相手側が新しい配線型を受け付けるかを 1 本ずつ見て**落とす
  （`Float` → `Int` は両方 SCALAR なので 1 本も落ちない。`set_params` の
  `vec4` ⟷ `color` と同じ性質）。パラメータの**値は引き継がない**
  （種別間に意味を保つ写像が無い。`default_parameter()` に戻す）
- **並び替えも `network.rs` に置いた**（`move_custom_port`）。
  「固定ポートを跨がない」は `is_fixed_port` を知っている層でしか
  判定できず、`Graph::reorder_ports` は生の置換のままにしたいため。
  固定ポートに当たったら移動はそこで止まり、呼び出しは成功する
- **Out 側の `Int` / `Bool` は読み戻せないので提示をやめた**。Out の
  カスタムポートはパラメータを持たない入力ポートで、3 つのスカラー種別が
  `accepted_types = [SCALAR]` に潰れる（`custom_port_type` は常に `Float`）。
  In では同名パラメータが種別を保つが、Out にはその置き場が無い。
  提示した選択が黙って別のものになるのが害なので、
  `allowed_for_out()` から `Int` / `Bool` を外して 8 種にした。wire 上は
  3 つとも `SCALAR` なので表現力は落ちず、REQ-LAYER-002 の「任意型」にも
  反しない。`allowed_for_in` は 3 種を出したまま
- ノードエディタ側の入口は `NodeEditorPanel::{add,remove,rename,move}_custom_port`
  と `set_custom_port_type`。いずれも `commit_graph` を通し、`Err` を
  呼び出し元へ返す（Properties がセクション下に理由を出す）。
  **単位 4 の右クリックはこの 5 つをそのまま呼ぶ**
- 「並び替えハンドル」は上下ボタンにした（ドラッグ並び替えは行内に
  Input と Select がある行では当たり判定が競合する）。ドラッグが要るなら
  単位 7 で入れ替える
- **名前 Input は Enter と blur の両方を報告する**。Enter の直後に blur が
  来ると同じ (旧名, 新名) が 2 回送られ、2 回目は既に改名済みのポートを
  探して `PortNotFound` になる — 成功した編集の下に失敗が出る。
  送った組を憶えて同一なら捨てる（`pending_color_commit` と同じ形。
  rebuild とターゲット切替で解除）。**単位 4 で同じ Input を使うなら
  同じガードが要る**
- 型 Select は既に選ばれている項目でも `Confirm` を出すので、
  「今と同じ型」は呼び出し側で弾く。弾かないとコアが同一グラフを返し、
  `commit_graph` が「undo しても何も変わらない」スナップショットを積む。
  移動ボタンは活性条件（隣が固定でない）がコアの停止条件と一致するので
  同種のガードは要らない

### 単位 4: ポート右クリック（ノードエディタ）

- ポートのコンテキストメニューに Rename / Delete
- 対象は In / Out / Subnet のカスタムポートのみ。固定ポートと通常ノードの
  ポートではメニュー項目を無効化する

**完了条件**

- 固定ポート上でメニュー項目が無効になるテスト
- 削除でエッジが消え、残りポートのエッジが保存される GPUI テスト

**実装で判明したこと**

- **Subnet のピンはこの単位の対象から外した。** 外側ピンは内部 In / Out から
  導出される実体であって、ピン自体を編集するのは筋が違う（編集すべきは
  内部 In のポート）。導出は単位 5 の `sync_subnet_pins` が担当し、
  `network::remove_custom_port` は `net.in` / `net.out` しか受け付けない。
  ピン上でも項目は出すが無効になる（`is_in_node` / `is_out_node` が偽なので
  特別扱いは不要）。**単位 5 が入った後に、ピン上の Rename / Delete を
  内部ポートへ転送するかを決める**
- メニューの出し分けは `port_menu_model(graph, PortHit)` という純関数に
  まとめた。無効の判定は `is_fixed_port` と In / Out 判定だけで、legacy `f` の
  例外もそこが持っているので UI 側に分岐が増えない。**項目は隠さず無効化する**
  — 出たり出なかったりする項目は、その操作が存在すること自体を教えない
- Rename の入力は **Outliner のレイヤー行改名と同じ一回きりの `InputState`**
  （Enter / blur で確定、Escape で破棄、focus はクリックハンドラが取る）。
  行が無いので、行の中ではなくポート位置に `deferred` + `anchored` で
  浮かせた（ノードエディタが検索パレットで既に使っている形）。
  リポジトリに Modal / ダイアログの前例は無いので新設しない
- 単位 3 が警告した Enter → blur の二重送信は、`take()` で編集を閉じてから
  コミットすることで消える（2 回目は編集が無いので何もしない）。
  **拒否されたときだけ**編集を開いたまま戻すので、そこには送った名前を
  憶えるガードが要る（同じ名前の blur は再試行せず閉じる）
- 拒否の理由は Properties と同じ文言。`port_error_message` を
  `panels/mod.rs` へ移して両パネルで共有し、ノードエディタでは
  キャンバス左下の通知として出す（セクションの下に相当する場所が無く、
  メッセージは特定のノードではなくパネルに属するため）。次のポート編集・
  次のコンテキストメニュー・ドキュメント更新で消える
- **ポート一覧が動くと、進行中のワイヤードラッグが黙って壊れる。**
  `DragMode::Connect` が持つ `PortHit` は **index** でポートを指していて、
  削除・並び替えは後続のポートを繰り上げる。`Graph::add_edge` は index も型も
  検証しないので、ずれた `PortHit` でドロップしても**失敗せず**、誰も読まない
  スロットへエッジが書かれる（評価器は未接続として扱う）。この計画が潰そうと
  している「静かな破壊」そのものなので、**ポート一覧が変わりうる編集で
  `Connect` ドラッグを取り消す**。index を持たない Pan / SelectBox /
  MoveNodes は触らない
- 取り消しの置き場所は 2 つ要る。`edit_custom_ports`（**5 つの操作すべての
  合流点**。Properties からの編集も `NodeEditorHandle` 経由でここを通る。
  この経路は `self.graph` へ直接書くのでドキュメント observer が差分を
  見つけられず、`refresh_from_document` は走らない）と、
  `refresh_from_document` のグラフが変わった分岐（undo / redo・別ウィンドウ）。
  改名エディタのアンカーも同じ再インデックスで古くなるので同じ場所で畳む
- **Delete はメニュー構築時の名前を実行時に引き直す。** メニュークリックが
  開いていた改名エディタを blur → 改名確定 させることがあり、そのとき名前は
  もう無い。index から引き直して「今そこにあるポート」を消すのは、ユーザーが
  指したのが名前であって枠ではない以上、破壊的な推測になる。**名前が消えて
  いたら何もしない**（何も壊れておらず、報告すべきエラーでもない）。
  ポートが在るのに拒否された場合だけ通知を出す
- 改名エディタを閉じるときは focus をパネルへ戻す（`dismiss_palette` と同じ形。
  `cx.defer` + ウィンドウ走査で、**そのエディタが実際に focus を持っている
  ときだけ**戻す）。拒否で開いたままにする経路では戻さない — その場で
  打ち直せることが、開いたままにする理由だから

### 単位 5: Subnet の生成と内部整合 — 実装済み

- `create_node` が `subnet` テンプレートに対して内部グラフ
  （`net.in` / `net.out` の空ペア）を生成する。Add Node からの生成で
  壊れたノードができないようにする
- `sync_subnet_pins(graph, subnet_id)`: 内部 In の出力から入力ピン、
  内部 Out の入力から出力ピンを再生成し、名前で remap、消えたピンのエッジを削除、
  promote 用パラメータを追従させる
- ロード時の正規化フックで全 subnet ノードに対して実行（ドリフト修復）
- `supports_param_ports`（`graph.rs:374`）が subnet を除外している現状は維持。
  promote パラメータはピン同期が管理し、`expose_param_port` とは別機構

**完了条件**

- Add Node で作った Subnet が評価でき（空の内部グラフでもエラーにしない）、
  ダブルクリックで潜れるテスト
- 内部 In のポートを追加・削除・並び替えしたとき、外側ピンと外側エッジが
  追随するテスト
- ピンがドリフトした状態のグラフをロードすると修復されるテスト
- `subnet.rs:87-95` の位置フォールバックに依存せず、名前一致で解決されることの
  テスト

**実装で判明したこと**

- **ピンの導出は In と Out で非対称になる**。内部 In の固定ポート
  （`base_geometry` / `t` / `f` / `source`）は評価側が
  `EvalContext` か囲みスコープの binding から答えるので、ピンにしても
  外側から読むものが無い。よって**入力ピンは内部 In のカスタム出力だけ**。
  一方 `net.out` の `frame` には固有の供給元が無く、`NetOutProcessor` は
  入力を集めるだけ・`SubnetProcessor` は名前で外側ピンへ写すだけなので、
  **出力ピンは内部 Out の入力ポート全部**（`frame` を含む）。
  `is_fixed_port` が両者を「固定」と呼ぶのは編集保護の話で、
  ピン境界の話ではない
- **新規 Subnet の内部 In は `t` だけを持つ**。`base_geometry` と `source` は
  殻の概念（ベースクアッド、下位スタックの合成結果）でサブネットには無く、
  `f` は決定事項どおりレイヤールート限定。`t` は
  どの入れ子階層でも継承される `EvalContext` から来るので正しく、
  ポート 0 個のノードが描かれるのも防げる。内部 Out は `frame` を持ち、
  これが**作った直後の Subnet が評価できる**理由になる
  （出力ピンが 1 つも無いと `SubnetProcessor` が名前で引けない）
- **promote パラメータは「ピンの関数」なので subnet ノードの
  パラメータ列を丸ごと同期が所有する**。`supports_param_ports` が subnet を
  除外している以上、そこに別経路でパラメータが載ることは無い。
  新規作成時の初期値は**内部 In の同名パラメータから種別が合えば複製**する
  — その値は promote 前に内部ネットワークが実際に評価していた既定値なので、
  ピンが生えた瞬間に評価結果が変わってはいけない。
  既存の値は種別が合う限り温存し、合わなくなったら
  `set_custom_port_type` と同じく新しい型の既定値に落とす
- **内部編集後の同期は `ravel_ui::document::replace_network` に置いた**。
  ノードエディタの `commit_to_document` はここを通り、
  `rebuild_subnets` が親を包み直す各段でピンを引き直す。呼び出し側の
  Document コミット 1 回に収まるので **1 操作 1 undo が保たれる**し、
  入れ子ネットワークを書く経路が他にできても自動で乗る
- **node id はサブネット内部も含めてグローバルに一意でなければならない**。
  `Evaluator` のプロセッサ表は `HashMap<NodeId, Arc<dyn NodeProcessor>>`
  （`eval.rs`）で**パスを含まない平坦な写像**なので、外側 subnet ノードと
  内部 `net.in` が同じ id を持つと 1 エントリを奪い合って一方が消える。
  `create_node` は呼び出し側から任意の id を受け取れる（テスト、
  パレットの接続可能性判定）ため、`seed_subnet_node` は
  **採った id が外側 id と一致したら捨てて次を採る**
- **id を発行する箇所と発行しない箇所を分けた**。`create_node` は
  `NodeId::next()` で内部 2 ノードを採る（`Document::id_watermarks` の
  `scan_graph` が `node.subnet` を再帰走査するので、ロード時の
  `advance_id_counters` が内部 id も追い越す）。逆に
  `sync_subnet_pins` は**一切 id を発行しない**: ロード時の正規化チェーンは
  `advance_id_counters` より**前**に走るため、そこで採った id は
  保存済み id と衝突しうる。結果として `subnet: None` の壊れたノードは
  同期では直らず、そのまま残る（未対応。修復には id 発行が要る）
- 内部グラフに In か Out が片方でも欠けていたら同期は**何もしない**。
  半分のネットワークからピンを導出すると、壊れた内部グラフを根拠に
  ユーザーの配線を消すことになる
- **`subnet.rs` の位置フォールバックは残した**。同期が入った以上、
  名前不一致は壊れたグラフでしか起きず、ロード時の修復が先に効く。
  外すと手組みのグラフ（テストフィクスチャ、`.ravprj` 移行の途中状態）が
  黙って評価不能になるだけで得が無い。「依存していない」ことは、
  単一出力ピンが内部 Out の**2 番目**のポートを名前で引くテストで示した
- ロードチェーンでの位置は `normalize_variadic_input_ports` の**後**
  （`validate_subnet_depth` より後という条件も満たす）。上流の 2 つが
  ポートそのものを動かすので、導出はそれが終わってからでなければならない

### 単位 6: Collapse / Extract

- `Collapse to Subnet`: 選択ノード群を内部グラフへ移し、境界を横切る
  エッジから内部 In / Out のポートを導出、外側の配線を新しいピンへ張り替える。
  ポート名は境界エッジの入力ポート名を採り、衝突は連番で回避する
- `Extract Subnet`: 逆操作。内部ノードを親へ戻し、ピンの配線を元のノードへ
  張り替える
- コマンドは `CommandId` と `workspace.rs` の `for_each_command!` 表に追加する
  （`.agents/rules/gpui.md` のコマンド経路単一性）
- In / Out ノード自身と synthetic ノードは選択に含まれても collapse の対象外
  （`node_editor.rs:3425-3427` の境界ノード不変条件を維持）

**完了条件**

- collapse → extract で元のグラフに戻るラウンドトリップテスト
- 境界を複数エッジが横切るとき、ポートが正しく導出され配線が保存されるテスト
- 名前衝突時に連番が付くテスト
- collapse が 1 undo で、In / Out / synthetic を巻き込まないテスト

### 単位 7: レジストリ / ロケール / 文書

- Ports セクション・メニュー項目・型名のロケール（`assets/locales/*.toml`）
- `docs/agent-api-reference.md` に新しい graph API とピン同期を記載
- `docs/ui-impl-status.md` の Properties / NodeEditor 表を更新
- REQ-LAYER-002 / 003 の受入条件を実装状況に合わせて更新

## 検証

- `mise run check`
- ポートの追加・削除・並び替え・改名について、エッジ保存の性質テストを
  `ravel-core` に置く（ウィンドウ不要）
- collapse / extract のラウンドトリップは `ravel-core` レベルで書ける形にし、
  GPUI テストはメニュー・ヒットテストに限る

## 非対象

- **HDA 相当の定義共有・インスタンス同期**。REQ-LAYER-003 で v2。移行パスは
  REQ-LAYER-009 が確保する
- **可変長ポート（variadic）とカスタムポートの統合**。variadic は
  `grow_variadic_input_group` が別機構として持つ。両者の統合は必要性が出てから
- **`layer.info` / `comp.info` のポート選択 UI**。単位 3 の Ports セクションを
  流用するが、ノード自体は `scene-info-nodes-plan.md` が担当する
