# GPUI Kit 移行計画（gpui-ce → gpui-pre、gpui-base の採用）

> **Status**: 計画のみ — 2026-09-04（調査は済み。`KIT-0` が go/no-go のゲート）

対象: Ravel が立つ GPUI 実装と、UI コンポーネント層の土台。
関連要件: REQ-UI-002/003/013（UI の作り込み）、REQ-GPU-001（device 共有）。
関連: `done/zero-copy-viewer-plan.md`、`done/gpu-device-loss-recovery-plan.md`、
`done/free-pane-docking-plan.md`。

## 問題

### 上流が別の GPUI に移り、今の構成では追随できない

`longbridge/gpui-component` は **`longbridge/gpui-kit`** に再編され、
3 層 + 拡張層に分かれた（すべて crates.io、0.6.0 は 2026-09-03 公開）。

| クレート | 中身 |
|---|---|
| `gpui-kit` | 傘。GPUI と有効な層を再エクスポートする |
| **`gpui-base`** | **無スタイルの振る舞い・状態・インフラ**（Apache-2.0） |
| `gpui-component` | 60+ のスタイル付きコンポーネント（base の上） |
| `gpui-shell` | Rust アプリ内の JS 拡張ランタイム |

問題は依存の噛み合わせである。

```text
gpui-kit / gpui-base 0.6  →  gpui = { package = "gpui-pre", version = "0.3.1" }
Ravel                     →  gpui = { git = narusenia/gpui-ce-ravel }   (crate 名 gpui, version 0.2.2)
```

`gpui-pre` は **Zed 自身の gpui のスナップショット公開**
（`gpui-pre snapshot of zed@5b055fa`、repo は zed-industries/zed）。
Ravel は **gpui-ce**（コミュニティフォーク）に立っている。

**`[patch]` では埋められない。** Cargo の規則は
「overridden されるコピーは *同じ名前と同じバージョン* を持たなければならない」で、
`package =` キーは同名クレートの複数版を patch するためのものであって
名前の付け替えには使えない。`gpui-pre` を gpui-ce に差し替えるには、
フォーク側のパッケージ名を `gpui-pre` に改名しバージョンも `^0.3.1` に
偽装したうえ、`gpui-pre-platform` / `-macros` / `-sum-tree` / `-web` /
`-reqwest-client` の兄弟すべてを揃える必要がある（gpui-ce に対応物が
無いものもある）。

**帰結: 今の構成のままだと Ravel は `gpui-component` 0.5.x に永久固定される。**

### そして gpui-ce が存在した理由が消えた

gpui-ce は「Zed の gpui が公開されていない」ことへの答えだった。
**`gpui-pre` が日次に近い頻度で公開されるようになった**（0.3.3 が 2026-09-03）
ので、その前提はもう成り立たない。Ravel のピンは 2026-07-30 のままである。

### 一方で Ravel が欲しいものは `gpui-base` にある

`gpui-base` が提供するのは無スタイルの振る舞い層である:
`actions` / `animation` / `component_traits` / **`dock`** /
**`input`（テキスト編集エンジン）** / `motion` / `slider` / `text` /
**`theme_tokens`**。フォーカストラップ、キーボードナビ、
**仮想リスト・ツリー**、スクロールバー、ウィンドウ級のテキスト選択、
undo / 履歴、カレンダー・カラーピッカー・OTP の状態機械。
「presentation styles は意図的に持たない」と明言されている。

これは `project-custom-ui-lib` の方針（gpui-component 脱却 → 独自 UI）と
衝突しない。**衝突しないどころか、その方針が必要としていた
「振る舞いだけ借りて見た目は自分で持つ」層がまさにこれである。**

## 調査結果（2026-09-04）

### フォークの 9 パッチのうち、上流に入っているのは 0 個

`gpui-pre` 0.3.3 と兄弟 4 クレート（`-macos` / `-linux` / `-windows` /
`-wgpu` / `-platform`、いずれも 0.3.3 公開済み）を横断して確認した。

| `narusenia/gpui-ce-ravel` のパッチ | 上流 | 移植の重さ |
|---|---|---|
| `set_always_on_top` | 無し | 軽（70 行 / 4 プラットフォーム） |
| minimize 通知（`observe_window_minimized`） | 無し | 軽（132 行） |
| `NativeGpuHandles`（Metal device / queue） | 無し。macOS 側に公開アクセサが 1 つも無い | 中（75 行） |
| **RGBA Metal テクスチャを surface に** | 無し。**上流の `SurfaceSource` は `Surface(CVPixelBuffer)` の 1 バリアントだけ** | **重（137 行 + Metal シェーダ）** |
| surface 完了通知（`SurfaceCompletion`） | 無し | 中（85 行） |
| **`gpu_context_full`** | 無し。**ただし `WgpuContext { pub instance, adapter, device, queue }` は上流で公開済み** | **軽くなった**（アクセサ配線だけ） |
| wgpu テクスチャ surface 完了 | 無し | 中（60 行） |
| Windows への wgpu surface 拡張 | 無し | 軽（27 行） |
| BGRA swizzle | 無し | 極小（6 行） |
| （`gpu_device_lost`） | 無し。**ただし `WgpuContext::device_lost()` は上流で公開済み** | **軽くなった** |

総量 **14 ファイル / +667 −87 行**。gpui-pre も gpui-ce と同じく
プラットフォーム別クレート + wgpu レンダラの構成なので、
**これは書き直しではなく移植**である。

### gpui-component 側のフォークは 4 パッチ → 2 に減る

現在の 4 パッチのうち **`fix: restore gpui-ce compatibility` は
「gpui-component に gpui-ce を噛ませる」ためだけに存在する**ので、
gpui-pre に移れば消滅する。残るのは「メニューの再フォーカス +
キーコンテキストの export」と「ヘッドレステストのガード 2 件」で、
**どれも上流に PR できる形**である。

### 44 ファイルの API 移行は軽い

`gpui_component` を名指しするファイルは 44（`ravel-app` 38 /
`ravel-dock` 4 / `ravel-ui` 1 / `ravel-project` 1）。0.6 の破壊的変更との
突き合わせ:

- **`Input` の 3 分割（Input / Textarea / Editor）→ 影響なし。**
  Ravel は単一行 `Input` / `InputState` / `InputEvent` しか使っていない
- **`Divider` → `Separator` → 影響なし**（未使用）
- **`Table` → `DataTable` → 2 箇所**
- `gpui_kit::init` ファサードへ寄せるのは任意（0.6 の `gpui-component` を
  直接使い続けても動く）

`ScrubInput` は既に Ravel 自前（`widgets`）で、フォークには無い。

## 目標構成

```text
                 ┌─ gpui-component 0.6（当面は既存箇所のみ）
gpui-pre 0.3.x ──┼─ gpui-base 0.6 ──→ ravel の独自コンポーネント（新規はここ）
  (+ Ravel の 9  └─ ravel-dock ──→ base の dock に寄せるか判断（KIT-4）
   パッチを移植)
```

**方向**: 新しい UI は `gpui-base` の振る舞いの上に Ravel の見た目で作る。
`gpui-component` は既に使っている箇所を動かし続けるためだけに残し、
**新規採用はしない**。

## 決定事項

### 段階移行はできない。リスクの段階化はできる

**gpui-ce と gpui-pre は 1 バイナリに共存できない** — `Entity<T>` / `App` /
`Window` が別の型になる（`MED-GPU-07` の wgpu 二重化と同じ構図）。
したがって Ravel 内の差し替えは 1 コミットである。

**ただしリスクは段階化できる**: 移植の成否は Ravel の外側（フォーク）で
先に決着させられる。`KIT-0` がその役目を負う。

### go/no-go は 1 点に絞る

**上流の `SurfaceSource` に Metal RGBA テクスチャと wgpu テクスチャの
バリアントを足せるか。** ここが通れば残り 8 パッチは機械的な移植である。
通らなければ移行そのものを見送り、gpui-ce に留まって
`gpui-component` 0.5.x で凍結する（そのとき `gpui-base` は
**読むだけ**の参考資料として扱う。Apache-2.0 なので設計を参照する自由はある）。

### `ravel-dock` を捨てるかは移行後に決める

`ravel-dock` は 2,190 行で、`gpui_component::dock` の配線を置き換えるために
書いた（`done/free-pane-docking-plan.md`）。`gpui-base` にも `dock` がある。
**移行と同時に判断しない** — 2 つの大きな変更を 1 つにすると、
どちらが壊したのか分からなくなる。`KIT-4` で独立に測る。

## 実装単位

### KIT-0: 移植の実証（Ravel を触らない）

**これがゲートで、実装単位ではない。** 成果物はフォーク 1 本と判断である。

- `narusenia/gpui-ce-ravel` の 9 パッチを `gpui-pre` 0.3.x へ移植した
  フォークを作る（別ブランチ / 別リポジトリ）
- 移植の順序は依存順: `SurfaceSource` の拡張 → Metal 経路 → wgpu 経路 →
  完了通知 → Windows → BGRA → window 系（always-on-top / minimize）
- `gpu_context_full` と device-loss 照会は**上流の公開 API に乗せ直す**
  （`WgpuContext` の `instance` / `adapter` / `device` / `queue` と
  `device_lost()` が既に pub なので、`PlatformWindow` トレイトへの
  アクセサ追加だけで済むはず）

**完了条件**

- フォーク単体で `cargo build` が macOS / Windows で通る
- **フォークの examples でゼロコピー surface が実際に描かれる**
  （テクスチャを渡して画面に出る。「コンパイルできた」は完了条件ではない）
- `WgpuContext` から device / queue を取り出す経路が
  `PlatformWindow` 越しに公開されている
- **移植できなかったパッチがあれば、それを列挙して go/no-go を宣言する**

### KIT-1: Ravel の土台差し替え（1 コミット）

- `Cargo.toml` の `gpui` / `gpui_platform` を `KIT-0` のフォークへ、
  `gpui-component` を 0.6 へ差し替える
- `[patch."https://github.com/zed-industries/zed"]` は不要になる
  （0.6 は crates.io の `gpui-pre` を見るので、`[patch.crates-io]` で
  `gpui-pre` を差し替える）
- 44 ファイルの API 追随（`Table` → `DataTable` の 2 箇所ほか）
- `ravel-gpu` の `interop` 境界は**変えない**。`.agents/rules/rust.md` が
  「バックエンド置換はこの署名を変える」と書いているとおり、
  変わるのは `interop::context_from_wgpu` に何を渡すかであって境界の位置ではない

**完了条件**

- `mise run check` が通る
- **`ZC-*` と `GPULOSS-*` のテストが全部通る**（ゼロコピー、device 採用、
  epoch 交換、pool lease、window lifecycle）
- **実機でゼロコピー Viewer が出る**（macOS。`ZC-2` の native interop 経路）
- `cargo build -p ravel-cli` が GUI / オーディオのフレームワークを
  リンクしないこと（`otool -L`）

### KIT-2: gpui-component フォークの棚卸し

- 残る 2 パッチ（メニュー再フォーカス + キーコンテキスト export、
  ヘッドレステストのガード）を上流 0.6 に対して作り直す
- **上流に PR を出す。** マージされればフォークが消える

**完了条件**

- フォークのパッチが 2 以下で、それぞれに上流 PR かその理由がある

### KIT-3: 最初の Ravel コンポーネントを gpui-base で作る

**この計画で一番大事な単位。** 土台を移した意味がここで出る。

- `gpui-base` の `component_traits` / `theme_tokens` に乗る
  Ravel 独自コンポーネントを 1 つ作り、**作り方を `docs/dev/` に残す**
- 最初の対象は `ScrubInput`（既に自前なので比較ができる）か、
  Properties のカーブ / ランプエディタ

**完了条件**

- 1 コンポーネントが `gpui-base` の振る舞いの上に載り、見た目は Ravel が持つ
- `docs/dev/` に「Ravel のコンポーネントを足す」ページがある
- **`gpui-component` の同等品を使うより何が良くなったかを書く**
  （良くならなかったなら、それも結論として書く）

### KIT-4: `ravel-dock` と base の dock の比較

- `gpui-base` の `dock` が `ravel-dock` の要求（分割 / タブ / D&D /
  N 窓ツリー / `PaneContent`）を満たすかを**測る**
- 満たすなら移行計画を別に切る。満たさないなら `ravel-dock` を残す理由を
  `free-pane-docking-plan.md` に追記する

**完了条件**

- どちらにするかの判断と根拠がある（コードは書かない単位でもよい）

### KIT-5: 文書更新

- `architecture.md` の「UI フレームワークのフォーク方針」を書き換える
- `docs/gpui-ui-guide.md` を 0.6 / base の語彙に合わせる
- `AGENTS.md` のリポジトリマップ（`ravel-dock` の説明は `KIT-4` の結果次第）

## やらないこと / 見送る選択肢

- **フォークを `gpui-pre` に改名して両取り**（前述の C 案）。Cargo に嘘を
  つく維持コストと、兄弟クレートの不足で成立しない
- **`gpui-shell`（JS 拡張）の採用**。Ravel の拡張点は
  `plugin-system-plan.md`（WGSL / WASM）と `ofx-host-plan.md` が持つ。
  スクリプト層を 3 つ目として今持ち込まない
- **`gpui-component` の新規採用**。既存箇所を動かすためだけに残す
- **移行と `ravel-dock` の置き換えを同時にやる**（前述）
- **`gpui-base` の見た目を借りる**。借りるのは振る舞いだけで、
  見た目を借りたら独自 UI にした意味がない

## 検証

- `KIT-0` はフォーク単体の build + examples の目視
- `KIT-1` は `mise run check` + `ZC-*` / `GPULOSS-*` + 実機のゼロコピー確認
- `KIT-3` 以降は各単位の完了条件
- **`KIT-1` は CI の両プラットフォーム（macOS / Windows）が緑になるまで
  マージしない。** cfg 跨ぎの変更が入るので手元だけでは足りない
