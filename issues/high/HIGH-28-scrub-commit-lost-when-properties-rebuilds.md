# HIGH-28 | bug | Properties のスクラブ中にウィジェットが作り直されると、ジェスチャ終端の `Commit` が失われ undo が効かない

`crates/ravel-app/src/panels/properties.rs:1802-1810`（`refresh_values_checked`）、
`:3068-3081`（スクラブの購読）、`:1339`（`fields_shape`）

## 症状

Properties で値をスクラブした後 **Undo が効かない**。値は変わったままで、
Undo を押すと**その 1 つ前の操作**が取り消される。
実機報告では「キーフレームを打っていると起きやすい」。

## 原因

スクラブは 2 段のジェスチャ契約になっている（`widgets/scrub_input.rs:32`）:

```rust
pub enum ScrubEvent {
    Change(f32),   // ドラッグ中。apply するが undo は記録しない
    Commit(f32),   // ドラッグ終了。ここで undo を記録する
}
```

購読はパネルが `self.scrubs` に `ScrubBinding { state, sub }` として**所有**する
（`:3081`）。`sub` が落ちればイベントは届かない。

一方 `refresh_values_checked`（`:1802`）は、文書が変わるたびに
フィールドの**形**を取り直し、変化していれば再構築を予約する:

```rust
let before = fields_shape(&self.sections);
self.refresh_values(cx);
if fields_shape(&self.sections) != before … {
    self.needs_rebuild = true;
}
```

`fields_shape` は `PropertyField` の**判別子**を指紋にする（`:1339`）。
つまり**ジェスチャの途中で `Change` が文書を書き換えた結果、その行の種別が
変わると再構築が走り、`ScrubBinding` ごと `sub` が落ちる**。
そのあとに来る `Commit` は誰にも届かない。

結果、`apply_document`（undo を記録しない）だけが走った状態で文書が確定し、
**undo スタックにはそのジェスチャの入口が無い**。Undo は 1 つ前の操作へ飛ぶ。

キーフレームを打っていると起きやすいのは、キーフレーム経路が
「定数 `Float` → チャンネル」の変換を含む（`:2104` の `toggle_key` の doc が
明言）ためで、**種別の変わる行がそこに集中している**。

## 関連

`LOW-APP-07`（デバウンスされた色コミットが破棄される）と**同じ型**の欠陥
— 保留中のコミットを、それを運ぶ入れ物ごと捨てている。
`CRIT-04`（未コミットジェスチャーの焼き付き、解決済み）とも隣接する。

## 修正方針

どちらかを取る。

1. **ジェスチャ中は再構築を遅らせる。** `needs_rebuild` を立てても、
   スクラブが進行中なら次のジェスチャ終端まで実行しない
2. **再構築の前に保留中のコミットを flush する。** `LOW-APP-07` の色と
   同じ扱いにして、両方を 1 箇所で解く

**1 が本筋**（ジェスチャの間は入れ物を捨てないのが契約）。ただしどちらでも
`LOW-APP-07` と一緒に直すこと — 片方だけ直すと同じ穴が色に残る。

回帰テストは、スクラブの `Change` → 種別が変わる → `Commit` の順で流し、
**undo が 1 回でスクラブ前の値に戻る**ことを見る。
