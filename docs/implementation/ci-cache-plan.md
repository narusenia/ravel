# CI キャッシュを sccache + R2 へ移す実装計画

> **Status**: Planned — 2026-08-08

対象: `.github/workflows/ci.yml`。要件文書は無い（開発基盤の改善）。

## 問題

**2 プラットフォームのキャッシュが 10 GB の上限に構造的に入らない。**

2026-08-08 の実測:

| 対象 | サイズ |
|---|---|
| macOS の `target/` + `~/.cargo` | 約 3.6 GB |
| Windows の同じもの | 約 7.3 GB |
| 合計 | **約 11 GB**（上限 10 GB） |

保存自体は動いている（main への push で
`Cache saved with key: macOS-cargo-…` が出る）。**問題は保存の後**で、
2 本が同時に存在できないため、マージのたびに片方が他方を追い出す。
実測時点で残っていたのは mise のキャッシュ 2 件のみ、Rust のキャッシュは
1 つも無かった。

`#211` は 3 本目（bench 専用の世代）を消して競合を 3→2 に減らしたが、
**2 本でも入らない**ので同じ状態に戻る。

**上限だけが問題ではない。** `target/` を丸ごと保存する形は、Windows で
**約 12 分**かかる（ci.yml のコメントが 7.31 GB で計測した値を記録している）。
仮に上限に収まっても、マージのたびにこの時間を払い続ける。

## 決定事項

### `target/` のアーカイブをやめ、sccache でコンパイル単位をキャッシュする

`actions/cache` は「ディレクトリを丸ごと固めて置く」道具で、Rust の
ビルド成果物とは相性が悪い。`target/` は再現に必要な量より遥かに大きく、
1 バイトでも変われば世代ごと作り直しになる。

sccache は**コンパイル単位ごと**にキャッシュするので:

- OS・ブランチ・PR をまたいで共有できる（同じクレートの同じフラグなら再利用）
- 保存が差分になり、12 分のアーカイブ工程が消える
- 10 GB の上限を持つ GitHub Actions キャッシュから離れられる

### バックエンドは Cloudflare R2（S3 互換）

- **エグレス無料**。CI は読み出しが圧倒的に多いので、ここが効く
- sccache は S3 互換バックエンドを標準で持つ（`SCCACHE_BUCKET` /
  `SCCACHE_ENDPOINT` / `SCCACHE_REGION=auto`）
- 保存量の上限が事実上無い。代わりに**ライフサイクルルールで期限を切る**
  （放置すると際限なく増えるため）

### `~/.cargo` のレジストリは `actions/cache` に残す

sccache はコンパイルをキャッシュするが、**クレートの取得はしない**。
`~/.cargo/registry` は数百 MB で 10 GB 上限に対して十分小さく、
`actions/cache` のままで無理がない。**`target/` だけを外す。**

## 前提（リポジトリ外の準備）

**これはリポジトリ側では用意できない。着手前に揃っている必要がある。**

1. R2 バケット（`ravel-ci-cache` 想定）
2. そのバケットに絞った R2 API トークン（Object Read & Write）
3. **ライフサイクルルール**（30 日で自動削除）
4. GitHub の Secrets: `R2_ACCOUNT_ID` / `R2_ACCESS_KEY_ID` /
   `R2_SECRET_ACCESS_KEY`。バケット名は Variables でよい

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| CICACHE-1 | sccache を導入し `target/` のアーカイブを外す | 上の前提 |
| CICACHE-2 | 効果の計測と設定の詰め | CICACHE-1 |

### 単位 1: sccache を導入し `target/` のアーカイブを外す

- `mozilla-actions/sccache-action` を入れ、`RUSTC_WRAPPER=sccache` を設定する
- R2 を S3 互換バックエンドとして構成する
  （`SCCACHE_BUCKET` / `SCCACHE_ENDPOINT` / `SCCACHE_REGION=auto`）
- `actions/cache` の path から **`target/` を外す**。`~/.cargo` は残す
- `actions/cache/save` の「main の push だけが書く」ゲートは
  **`~/.cargo` に対しては残す**（レジストリは滅多に変わらないので、
  PR ごとに書く理由が無い）
- **Secrets を持たない実行でも壊れないこと。** フォークからの PR には
  Secrets が渡らないので、そのときは sccache 無しで普通にビルドする
  （`RUSTC_WRAPPER` を設定しない分岐）

**完了条件**

- main への push と PR の両方で CI が緑
- `sccache --show-stats` の出力を各ジョブの最後に出し、
  **2 回目以降の実行でヒット率が 0 でない**ことを確認できる
- **Secrets の無い実行（フォーク PR を模した実行）で CI が緑**
- GitHub Actions のキャッシュ使用量が **10 GB を大きく下回る**
- Windows の保存工程（約 12 分）が消えていること

### 単位 2: 効果の計測と設定の詰め

- cold / warm の実測を取り、`ci.yml` のコメントを実測値で更新する
  （現在のコメントは `target/` 方式の数字なので、残すと誤解を招く）
- ヒット率が低いなら原因を切り分ける（`SCCACHE_C_CUSTOM_CACHE_BUSTER`、
  デバッグ情報のパス埋め込み、`-C incremental` の扱い）
- R2 の使用量を確認し、ライフサイクルの期間を決め直す

**完了条件**

- cold / warm の所要時間が `ci.yml` に実測値で記録されている
- 30 日運用したときの R2 使用量の見積もりが書かれている

## 非対象

- **セルフホストランナー。** 効果は大きいが運用が増える
- **`cargo-chef` などの依存段階分離。** sccache で足りるかを先に見る
- **ローカル開発でのキャッシュ共有。** CI の話に閉じる
