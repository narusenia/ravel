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
