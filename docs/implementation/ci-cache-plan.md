# CI キャッシュを sccache + R2 へ移す実装計画

> **Status**: CICACHE-1 実装済み / CICACHE-2 実測済み — 2026-08-08。CICACHE-3 実測の結果 kache へ置き換え — 2026-11-24

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
| CICACHE-3 | sccache を kache に置き換える（計測して勝ったときだけ） | CICACHE-2 |
| CICACHE-4 | `D:` が ReFS になった後の Windows 再計測 | CICACHE-3、runner image の切り替え（2026-11-30） |

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

#### 実測（2026-08-08）

`check` ジョブの wall clock。cold は sccache 導入 PR（バケット空）、
warm はその後の main への push。

| | cold | warm | 削減 |
|---|---|---|---|
| macOS | 22分26秒 | 16分55秒 | **-25%** |
| Windows | 29分37秒 | 21分19秒 | **-28%** |

warm のヒット率は macOS **99.61%**（1843 要求中 6 ミス）、
Windows **91.90%**（1490 要求中）。cold は 0.07% / 0.00% だったので、
バックエンドは効いている。

**`target/` 方式で約12分だった Windows の保存工程は 37 秒**（macOS 12 秒）。
当初の目的は達成。

warm ジョブの内訳は Clippy 426s / Test 548s（macOS）、459s / 740s（Windows）に対し
**キャッシュの入出力は 13〜38 秒**。つまり**残りは素のコンパイルとテスト実行**で、
キャッシュをこれ以上大きくしても縮まない。

#### デバッグ情報を CI でだけ落とす

同一マシン・同一コマンド（`cargo test --workspace --all-targets --no-run`）で
交互に測った:

| debug | wall | `target/` | sccache 格納量 |
|---|---|---|---|
| full（既定） | 207.8 / 203.2 秒 | 7.4 GB | 795 MB |
| `line-tables-only` | 182.5 秒 | 6.0 GB | **667 MB** |

サイズは再実行でバイト一致、時間差 約11% は full 同士のばらつき 約2% の外。
**オブジェクトが 16% 小さくなる**のが本題で、warm ジョブは
それを読み戻す時間（平均 0.542 秒 / 回、macOS）に支配されているため、
ペイロードの縮小がそのまま効く。

`[profile.dev]` ではなく `ci.yml` の `CARGO_PROFILE_DEV_DEBUG` で設定する
（ローカルのデバッグ体験を落とさない）。**変更するとバケットが一度全ミスになる**
— フラグがハッシュに入るため。

**CI での確認（PR #339 の実行）**: 予告どおりヒット率 0.13% / 0.00% の完全 cold に
なり、その状態で **macOS 18分 / Windows 22分**。変更前の cold（22分26秒 /
29分37秒）と比べて **-20% / -26%**。cold 同士なのでキャッシュの効きではなく
**デバッグ情報の量そのものが効いている**ことの確認になる。ローカルの 11% より
大きいのは、CI ランナーの方がディスクと帯域が細いため。

#### R2 使用量の見積もり（30 日）

ローカル実測のオブジェクト単価と、CI のミス数から積む。

- **単価**: 692 コンパイル → 795 MB、すなわち **約 1.15 MB/オブジェクト**
  （`line-tables-only` なら約 0.96 MB）。ローカルは sccache 0.15.0 / arm64、
  CI は 0.17.0 なので**桁の見積もり**として扱う
- **初回充填**: cold の書き込みが macOS 1520 + Windows 1173 + bench 882
  ＝ 約 3575 オブジェクト → **約 4.1 GB**
- **1 マージあたりの増分**: warm のミスが macOS 6 + Windows 95 ＝ 約 101、
  bench を足して 約150 と置くと **約 170 MB**
- **頻度**: 直近 30 日で main へのマージ **114 件**（約 3.8 件/日）。
  PR への push も同程度書くので、保守的に 2 倍で見る

→ 30 日で **約 25〜45 GB** に落ち着く。R2 の料金では保管が月 $0.4〜0.7、
Class A/B の操作を足しても **月 $1 前後**（エグレス無料）。

**ライフサイクルは 30 日のまま据え置く。** 費用は延ばしても誤差だが、
3.8 マージ/日の変更頻度では 30 日より古いオブジェクトが要求される見込みが薄く、
延ばす利得が無い。

#### 追って見るもの（この単位では判断しない）

- `bench` ジョブは 882 書き込みでヒット率 0.00% だったが、これは
  **sccache 導入後の最初の bench 実行**（cold）なので当然。2 回目の実測が
  出るまで、bench がキャッシュから得をしているかは判断できない
- Windows の 91.90% が systematic かどうかも warm 1 サンプルでは決まらない

### 単位 3: sccache を kache に置き換える（計測して勝ったときだけ）

[kache](https://github.com/kunobi-ninja/kache)（Apache-2.0）は sccache と同じ
`RUSTC_WRAPPER` 型のコンパイラキャッシュで、**sccache が素通しするものも
キャッシュする**: proc macro・ビルドスクリプト・テストバイナリなど、リンカを
通るクレート。CICACHE-2 の warm でも Clippy 426 秒・Test 548 秒（macOS）が
残っていて、その一部はこの「素通し」分のコンパイル。ここが縮む見込みが
置き換えの理由（どれだけ縮むかは測るまで分からない）。

**計測してから置き換える。** v2 系は 2026-11 時点でリリースから 1 か月ほどで、
ほぼ毎日リリースされている。ベンダーの比較ページも速度の順位付けはしておらず、
「自分のビルドで測れ」としている。だから PR の上で測り、負けたら PR を閉じて
sccache に残る。

#### 構成

- `kunobi-ninja/kache-action@v1`、kache 本体は `v2.3.0` に固定。理由は
  sccache を固定したのと同じ（ハッシュの計算方法が変わると全エントリが外れる）
- R2 は同じバケットを使い、`s3-prefix: kache` で sccache の残骸（バケット直下）と
  分ける。sccache のオブジェクトは既存のライフサイクルルール（30 日）で消える。
  **kache 自身は remote のオブジェクトを消さない**ので、このルールは外さない
- `cache-executables: "true"`。action の既定はオフで（Windows では kache 本体でも
  既定オフ）、これを入れないと置き換える意味がほぼ無い
- `cache-dir: ${{ runner.temp }}/kache`。Windows ランナーはホームと作業領域が
  別ボリュームで、同じボリュームに置かないと復元がリンクでなくコピーになる
- **書き込み方針が sccache と違う。** kache が共有 prefix に書くのは保護ブランチへの
  push だけ（`main` は ruleset `protect-default` で保護済み）。PR は既定で読むだけ
  なので、同じ PR の 2 回目の push が 1 回目の成果を使えない。
  `KACHE_PULL_REQUEST_PREFIX: kache-pr` を設定し、PR は共有 prefix を先に読み、
  自分のビルド結果を `kache-pr` に書く
- 統計は action の post ステップがジョブサマリに書く。ログにも残すため
  `kache stats --last-build` のステップを置く

#### 計測の手順

1. 置き換え PR の最初の push = **cold**（`kache/` は空、結果は `kache-pr/` へ）
2. 同じ PR をもう一度走らせる = **warm**（`kache-pr/` から読む）
3. `check` ジョブの wall clock・Clippy / Test ステップ・ヒット率を、CICACHE-2 の
   sccache の数字（cold 22分26秒 / 29分37秒、warm 16分55秒 / 21分19秒）と比べる

**採否の基準**: macOS / Windows **両方で** warm が sccache の warm 以下なら採る。
片方でも遅ければ PR を閉じて sccache に残り、数字をこの節に残す。

マージ後の最初の main push は共有 prefix に対して cold になる（main は
`kache-pr/` を読まない）。それ以降の PR がそれを読む。

#### 実測（2026-11-24、PR #648）

`check` ジョブの wall clock。cold は空の `kache/` prefix に対する最初の実行、
warm はその再実行（`kache-pr/` から読む）。sccache は CICACHE-2 の値。

| | キャッシュ無し | cold | warm | sccache warm 比 |
|---|---|---|---|---|
| macOS | 20分10秒 | 16分37秒 | **7分02秒** | **-58%**（16分55秒） |
| Windows | 27分01秒 | 38分40秒 | **12分59秒** | **-39%**（21分19秒） |

「キャッシュ無し」は PR 本文に action のオプトアウト記法が含まれていて kache が
入らなかった実行で、偶然取れた基準値。

- warm は 3 サンプル: macOS 7分02秒 / 9分35秒 / 25分34秒、Windows 12分59秒 /
  12分49秒 / 12分29秒。macOS の 25 分は、保存済みのクレート 151 個を
  コンパイルしてから保存済みと分かった（kache の「Duplicates」）実行で、
  remote のエントリが cargo の要求に間に合わなかった。macOS の warm は
  「7〜10 分、ときどき外れ値」と読む。sccache の warm（16分55秒）より遅かったのは
  この 1 回だけ
- warm のヒット率: macOS 94.9%（コンパイル時間比 98.7%）、Windows 99.6%（99.9%）
- 効いたのは見込みどおり**リンクを通る成果物**。warm の Test ステップが
  macOS 548 秒 → 187 秒、Windows 740 秒 → 378 秒。Clippy も
  426 秒 → 205 秒、459 秒 → 349 秒
- **採否: 採る**（両プラットフォームで sccache warm を下回った）

残った問題:

- **Windows の cold は キャッシュ無しより約 12 分遅い。** 全部コンパイルした上で
  約 3 GiB をアップロードするため。cold は prefix が空のとき（kache の
  バージョンやフラグを変えた直後）だけ起きる
- **Windows の復元がリンクでなくコピー**（warm で 13.3 GiB）。計測時の `D:` は
  NTFS で、kache は NTFS ではコピーする（ハードリンクは危険側の
  `windows_hardlink` を入れたときだけで、入れていない）。runner image は
  **2026-11-30 から `D:` を ReFS にする**（actions/runner-images#15315）。ReFS では
  復元がクローンになる一方、kache はキーの入力を読み直すので、差し引きは
  分からない。**切り替え後に Windows を測り直す**
- **R2 の使用量**: 1 プラットフォームの全量で約 3 GiB（圧縮後）。アップロードは
  内容で重複排除され、warm 再実行のアップロードは数 MiB だった。定常の増え方は
  各 PR が変えた分。sccache（1 実行 ≈ 0.7 GB）より大きいが、30 日の
  ライフサイクルで頭打ちになる
- **PR 本文の `[no-cache]` 記法**: action は PR 本文にこの文字列があると何も
  入れずに抜ける（ビルドはキャッシュ無しで通る）。統計ステップは
  `RUSTC_WRAPPER` で守ってあるので赤にはならない

#### ローカル

CI とは独立に、開発機も `kache init` で sccache から移せる
（`~/.cargo/config.toml` の `rustc-wrapper` を置き換え、常駐プロセスを login
service に登録する）。リポジトリには何も足さない（`.kache.toml` を置くと
ユーザー設定と**マージされずに置き換わる**ため、action が書く remote 設定を
消してしまう）。

**完了条件**

- cold / warm の実測が本節と `ci.yml` のコメントに記録されている ✅
- 採否が基準に沿って決まり、採った場合は sccache の記述が `ci.yml` から消えている ✅

### 単位 4: `D:` が ReFS になった後の Windows 再計測

CICACHE-3 の Windows は `D:` が NTFS で、復元が全部コピーだった（warm で
13.3 GiB）。runner image が 2026-11-30 から `D:` を ReFS にする
（actions/runner-images#15315）。ReFS では復元がクローンになるが、kache は
ReFS でキーの入力を読み直す。

- 切り替え後の main push（warm）で `check (windows-latest)` の wall clock、
  `kache stats` の Restored 行（clone / copy の内訳）とキー計算時間を取る
- CICACHE-3 の warm（12分59秒）より遅くなっていたら原因を切り分ける

**完了条件**

- ReFS での warm の実測が本計画と `ci.yml` のコメントに記録されている

## 非対象

- **セルフホストランナー。** 効果は大きいが運用が増える
- **`cargo-chef` などの依存段階分離。** sccache で足りるかを先に見る
- **ローカル開発でのキャッシュ共有。** CI の話に閉じる
