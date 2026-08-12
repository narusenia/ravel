# Outliner 仕様

> 最終更新: 2026-07-30 ／ 索引: [`../ui-spec.md`](../ui-spec.md)

プロジェクト構造ビュー。Composition → Layer → Node の 3 階層。
関連要件: REQ-UI-013。設計は
[`../../implementation/done/outliner-comp-management-plan.md`](../../implementation/done/outliner-comp-management-plan.md)。

Timeline が「アクティブコンプの時間ビュー」、Outliner が「プロジェクトの
構造ビュー」という分担。

```text
▼ ▣ Comp 1  1920×1080 30fps 300f (active)
    ▸ ● title                     ← レイヤー行
    ▼ ● circle_array
        ◇ rasterize               ← net.out を根に上流を深さ優先で展開
            ◇ geometry.transform
                ◇ shape.polygon
            ◇ generate.solid
        ─ Unused (1)              ← net.out に到達しないノード
            ◇ math.scalar
      ● child_dot                 ← parenting（表示のみ）
▼ ▣ Comp 2  1080×1080 30fps 120f
      ● bg （薄い表示）            ← 非アクティブコンプの子行
```

## 行の構成

- **レイヤー配下のノード行**: `net.out` を根に上流を深さ優先で展開する
  （`net.out` 自身は非表示）。同一ネットワーク内で既出のノードは参照マーク付きの
  葉にする（DAG の指数膨張を防ぐ）。`net.out` に到達しないノードは末尾の
  Unused グループへ。行順はエッジの入力ポート順（Node Editor が入力を描く順）
- **サブネットノード**はバッジ付きで表示し、ダブルクリックは Node Editor の
  dive に委譲する。平坦化しないのは**内側のネットワーク**だけで、サブネット
  ノード自身の上流入力は外側のノードなので通常どおり展開する
- **非アクティブコンプの子行**は薄く表示する

## 操作

| 操作 | 挙動 |
|---|---|
| コンプ行シングルクリック | 選択（Properties に `Composition` が出る。コンプ管理コマンドの対象にもなる） |
| コンプ行ダブルクリック | アクティブコンプを切り替える |
| レイヤー / ノード行シングルクリック | 選択 |
| レイヤー行ダブルクリック | Node Editor でネットワーク全体を fit |
| サブネットノード行ダブルクリック | Node Editor で dive |
| 非アクティブコンプの子行 | シングルは無反応、ダブルでアクティブ切替 + 選択 |
| 複数選択 | Shift で範囲、Cmd でトグル。Duplicate / Delete は選択全体に 1 undo |
| レイヤー行の縦ドラッグ | スタックの並べ替え（同一コンポジション内） |
| 右クリック | Rename / Duplicate / Delete（レイヤー）、Settings / Duplicate / Delete（コンプ） |
| コンプ行右クリック ▸ レイヤーを追加 | 組み込みテンプレート（Solid / Shape / Video / Audio / Null）。**アクティブコンプの行にだけ出す**（レイヤー追加はアクティブコンプが対象なので、別のコンプの行に出すと違う場所に足すことになる） |

## 選択の一元化

レイヤー選択は `LayerSelection`、ノード選択は `CanvasSelection`、アクティブコンプは
`ActiveComposition`。Timeline / Outliner / Node Editor / Properties は同じ Global を
読み書きするので、パネル間の双方向同期プロトコルを持たない。

**不変条件: `LayerSelection.comp == ActiveComposition`。**

## 未実装項目

| 項目 | 担当 |
|---|---|
| 親子付けの変更（D&D または他手段）。**表示のみ** | `SHELL-5`（Properties のドロップダウンとして入れる） |
| 検索・フィルタ欄 | 未計画 |
| MediaBin の行再構築の削減（Outliner 側は解決済み） | `MED-UI-05`（フェーズ C3） |
