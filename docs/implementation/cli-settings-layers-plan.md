# CLI 設定層の解決計画

> **Status**: 実装完了 — 2026-08-11

## 背景

`ravel-cli render` は `ResolvedSettings::default()` だけから
`SharedCacheBudget` を作るため、GUI と同じ `settings.toml` および
`.ravprj` の `[cache]` 設定がヘッドレス経路に届かない。GUI 側で導入済みの
キャッシュ設定の検証も、`ravel-app` に閉じている。

## 目標アーキテクチャ

```text
global settings.toml ─┐
                       ├→ ravel-project::settings → validated CacheBudgetConfig
project .ravprj ──────┘                              → SharedCacheBudget
```

純粋な設定ファイル読み込みとキャッシュ検証は GUI-free な `ravel-project` に
置く。`ravel-app` は既存の GPUI / セッション境界の薄いラッパーを保ち、
`ravel-cli` は既にロードした `ProjectFile` の global → project 解決結果を
同じ検証関数へ渡す。ディスク層の課金経路は追加しない。

## 実装単位

### CLISET-1: 共有設定読み込みとキャッシュ検証

- `ravel-project::settings` に global 設定の読み込み入口を追加する。
- `MIN_CACHE_LIMIT_MB` / `MAX_CACHE_LIMIT_MB`、範囲判定、sim 予約率判定、
  絶対パス判定、範囲外を既定値へ戻す予算変換を移す。
- `ravel-app` は既存の API と設定ダイアログの挙動を保ち、移設先を参照する。

### CLISET-2: CLI の設定層配線

- レンダー対象の `ProjectFile` を global 層と project 層で解決する。
- 解決結果を共有検証関数に通して `SharedCacheBudget` を構築する。
- CLI の上書きフラグおよび `Tier::Disk` の課金経路は追加しない。

### CLISET-3: 回帰検証と課題台帳

- global / project の上限が headless 予算へ届くことをテストする。
- 範囲外の上限と相対 `cache.root` が GUI と同じ共有判定で拒否されることを
  テストする。
- GUI の既存キャッシュ設定テストを無変更で通し、課題を closed へ移す。

## 完了条件

- `cargo test -p ravel-cli` と GUI の `settings_cache.rs` 13 件が通る。
- `mise run check` と `mise run docs:check` が通る。
- 変更は本計画の単位と issue 台帳の更新に限定する。
