# closed / low — 解決済みの軽微な項目

未解決分は [`../low/backlog.md`](../low/backlog.md)。

---

**LOW-GPU-01 | bug | `read_texture` の容量計算が 16384×16384 RGBA32F で u32 オーバーフロー**
（**解決済み**: フェーズ A2。容量計算は `u64` に広げ、`checked_mul` を通す
（`crates/ravel-gpu/src/transfer.rs:160-163`））
`crates/ravel-gpu/src/transfer.rs:210`
`Vec::with_capacity((unpadded_bpr * key.height) as usize)` が u32 同士を掛ける。
16384px × 16B = 262144 bpr × 16384 行 = ちょうど 2³² → debug ビルドで overflow panic、
release では容量ヒントが 0。直上の `buffer_size` は正しく u64 を使っている。
クレートは「人為的な解像度制限なし」を謳い、アダプタの `max_texture_dimension_2d` を
そのまま要求する（`device.rs:72`）ので到達可能。
→ `unpadded_bpr as usize * key.height as usize`

**LOW-AUD-01 | debt | prep スレッドのコメントが存在しない送信タイムアウトを約束している**
（**解決済み**: フェーズ A3。`chunk_tx.send` は `select_biased!` の 1 ブランチになり、
キューが満杯でもコマンド受信が先に成立する）
`crates/ravel-audio/src/engine.rs:284-291`
`chunk_tx.send` のコメントは「コマンドに応答できるようタイムアウトを使う」と書くが、
呼び出しはブロッキングの `send`。キューが満杯の間 Pause / Seek / SetTrack が
最大1チャンク（約 21ms）待つ。現状は無害だが、コードが持たない挙動を文書化しており、
将来キュー深さやチャンクサイズを増やすとコマンドレイテンシが無言で増える。
→ コメントどおり `send_timeout` にしてタイムアウト時にコマンドチャネルを再チェックする。
またはコメントを直す。

**LOW-APP-01 | bug | Duplicate がコピー用クリップボードを破壊する**
（**解決済み**: フェーズ A2。回帰テストは
`duplicate_does_not_replace_the_copy_clipboard`（`crates/ravel-app/src/panels/node_editor.rs:4396`））
`crates/ravel-app/src/panels/node_editor.rs:1107-1114`
Duplicate = copy + paste の実装なので、A をコピー → B を Duplicate → Paste で B が貼られる。
→ `self.clipboard` に触らず一時的な `ClipboardContent` から paste する。

**LOW-APP-14 | debt | 分離ウィンドウの配置永続化が未達の契約**
（**解決済み**: PR #242（2026-08-01）。各ウィンドウホストが `observe_window_bounds` で
自分の配置をレイアウトへ記録し（I/O なし）、`layout_persist` が
`<config>/ravel/layout.toml` へ書き出す。復元は `window_host::window_bounds_for` の
1 箇所で、`WindowPlacement::is_usable()`（有限値・最小サイズ）を通り、かつ
**接続中のディスプレイに掛かる**記録だけを信用する（サイズ 0・非有限値、および
外部モニタを外した後の画面外の記録は既定サイズで中央に開く）。設計は
`docs/implementation/done/free-pane-docking-plan.md` の `DOCK-9`）
`crates/ravel-ui/src/window.rs:20-33`, `:100-113`
`WindowPlacement` / `set_placement`（「セッション間で復元される」）に呼び出し元がゼロ。
配置を記録も復元もしていない。
→ 配線するか削除する。

**LOW-APP-17 | debt | ログの不整合**
（**解決済み**: PR #236（2026-08-01）。該当関数ごと消えた。分離ウィンドウの生成と
クローズは `window_host` に移り、失敗経路はすべて `tracing::error!` /
`tracing::warn!`。`ravel-app` に残る `eprintln!` は `main.rs` の i18n 初期化失敗
（tracing の subscriber を入れる前）と `examples/` だけ。計画上は
`DOCK-8` の削除範囲だったが、`DOCK-6` が旧 detach 経路を置き換えた時点で
到達不能になったのでそこで消えた）
`crates/ravel-app/src/workspace.rs:603`, `:1228` が分離ウィンドウ失敗に `eprintln!` を使う
（他はすべて `tracing`）。
→ `tracing::error!` に変更。

**LOW-APP-15 | debt | ユーザーのキーバインドカスタマイズが読み込めない**
（**解決済み**: PR #277（2026-08-03）。起動時に `<config_base>/ravel/keybindings.toml` を
埋め込み既定へ重ねて読む（`crates/ravel-app/src/keybindings.rs`）。寛容な入り口
`overlay_user_toml` を `parser.rs` に足し、壊れた 1 行はその行だけ捨てて起動を止めない。
バインドは `AppShell` 経由で登録されるのでユーザー由来も `!Input` コンテキストが付き
（`MED-APP-16` の回帰枠は `crates/ravel-app/tests/keybinding_overrides.rs` の 3 本）、
環境設定に読み取り専用の一覧が出る。画面からの割り当て編集は `SET-12`）
`crates/ravel-ui/src/keybindings/parser.rs:71-146`, `crates/ravel-app/src/main.rs:70`
パーサーは TOML / JSON ファイルをサポートし、ドキュメントは完全なカスタマイズを謳うが、
アプリは `AppShell::default()` 経由で埋め込みの `default.toml` のみを読む。ユーザーパスを読まない。
→ 起動時に設定ディレクトリのユーザーキーバインドファイルをデフォルトに重ねて読み込む。

**LOW-APP-11 | debt | i18n の穴 — ハードコードされたユーザー向け英語**
（**解決済み**: PR #308（2026-08-06）。語で名づけられるもの
（`Network (N nodes)` / `Audio` / `Null` / `{n} frames` / `Edge Style` /
チャネル名 `Value`・回転・不透明度・ゲイン）をロケールキー化し、
**記法**（単位記号 `f` / `fps`、トグルグリフ `S`/`M`/`L`/`F`、軸と
カラーチャネルの `X`/`Y`/`R`/`G`/`B`/`A`）は訳さないものとして
`docs/specifications/ui/timeline.md` の「翻訳しない表記」節に規約化した
— issue 本文が許していた 2 択の後者。数を含む行は `ravel-ui` が
i18n に依存しないため `properties::counted_value` でキーと数を一緒に載せ、
表示境界が `{count}` を埋める（**保存値はキーのまま**なので言語切替が
編集結果を変えない）。複数形機構は入れないと決め、その規約を
`docs/dev/add-locale.md` に明記した — 英語の `1 nodes` / `1 frames` は
現行表示の保存を優先して残っている。en / ja とも 595 キーで差分ゼロ）
機構は存在するが以下が迂回している。
- `crates/ravel-ui/src/properties/layer.rs:195-199`, `:325` — Properties に出る
  "Network (N nodes)" / "Audio" / "Null" / "{n} frames"
  （このファイルは `VALUE_ON` / `VALUE_OFF` でロケールキーのパターンを既に定義している）
- `crates/ravel-app/src/panels/node_editor.rs:2145` — `.submenu("Edge Style", …)` の生リテラル。
  子項目はローカライズ済みでキーも存在する
- `crates/ravel-app/src/panels/timeline.rs:2237`, `:2244-2246`, `:3137/3157/3177`, `:3541` —
  "{playhead}f"、"{fps} fps · {n}f" の単位リテラルと S/M/L/F トグルのグリフ
  （ツールチップはローカライズ済み、グリフは未）。
  `ravel-ui/keyframes.rs:688-703` 由来のチャンネル名 "Value"/"X"/"Y"… も未翻訳で描画される

→ ロケールキーを追加する（または軸の文字は意図的な記法として文書化する）。

**LOW-APP-07 | bug | デバウンスされた色コミットが破棄され、ライブプレビューが無関係な undo ステップに畳み込まれる**
（**解決済み**: PR #344。`HIGH-28` / `MED-APP-30` と同じ規律でまとめて解いた。
`flush_pending_color_commit` がスロットのクリア・上書きの前に走り、
ターゲット切替では `end_gestures` が旧ターゲットのまま確定する）
`crates/ravel-app/src/panels/properties.rs:566-571`, `:1002-1028`
300ms の静穏ウィンドウ内でターゲット切替または2回目の色ジェスチャーが起きると、
`apply_document` は既に行われた後で pending コミットが破棄される。
→ スロットをクリア / 上書きする前に pending コミットを flush。
