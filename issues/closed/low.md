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

**LOW-APP-24 | bug | Collapse to Subnet が `net.out` の予約名 `frame` を採番から除外していない**
（**解決済み**: PR #348（2026-08-09）。`outbound_names` を `[PORT_FRAME]` で seed し、
inbound 側と対称にした。衝突した pin は `frame_2` になる）
`crates/ravel-core/src/network.rs:1566`（`outbound_names` が空の `HashSet` で始まる）
入力ポート名が `frame` のノードへ抜けるエッジを畳むと、`net.out` に `frame` という
名前の入力ポートが作られる。`is_fixed_port`（`:441`）がそれを fixed と判定するので、
**ユーザーは削除もリネームもできない**。型も本来の `frame` と違いうる。
inbound 側（`:1561-1565`）は `PORT_BASE_GEOMETRY` / `PORT_TIME` /
`PORT_FRAME_INDEX` / `PORT_SOURCE` で正しく seed され、コメントもその理由を
書いている。**outbound だけが対称性を欠いている。**
→ `outbound_names` を `[PORT_FRAME]` で seed する。1 行。

**LOW-APP-08 | bug | 音声のリリンク / オフライン staleness（**latent ではなくなった** — #469 で到達可能）**
（**解決済み**: `CacheKey` / `TrackSpec` に解決済みパスを持たせた。
`AudioMixdown::desired_tracks` が `Document` から各アセットの `resolved` を引き、
キー・spec の等価性・`build_key` の 3 つ全部にパスが乗る。オフラインは
`resolved: None` のキーになるので、リリンクすると別キーとして再試行される。
古いパスのエントリは `AudioService::drop_superseded` が捨てる。回帰テストは
`relinking_the_asset_replaces_the_cached_audio`
（`crates/ravel-app/tests/audio_playback.rs`）、
`the_resolved_file_comes_from_the_document_and_keys_the_cache`
（`crates/ravel-audio/src/mixdown.rs`）、
`a_relink_drops_only_the_previous_files_entries`
（`crates/ravel-app/src/audio/mod.rs`））
`crates/ravel-audio/src/mixdown.rs:53`（`CacheKey`）、`crates/ravel-app/src/audio/mod.rs:106`・`:177`・`:386`
`CacheKey` が `(asset_id, stream_index)` で**解決済みパスを含まない**。
起票時は「将来リリンクを実装すると」という latent な指摘だったが、
**`MEDIA-6`（#469）で Relink が入ったので今日到達する**:
音声素材を Relink しても、そのセッションの間は**古いデコード結果が
キャッシュから返り続ける**（映像側は `FrameKey::image(path, …)` が
パス鍵なので影響を受けない — 音声だけが例外）。
一度オフラインになったアセットはセッション中に再試行されない
（モジュールコメントは逆のことを書いている）。
**深刻度は low のままにしてあるが、実質「利用者の操作のあとに出る音が
古い」= 出力の誤り**なので、着手順は low の並びではなく
`roadmap.md` の基準 0 として扱う。
→ キーに解決済みパスを含める / `resolved` の変更時に失敗エントリを消す。
コメントを直す。**`ravel-audio` 側の `CacheKey` に手が入るので、
`ravel-app` の 3 箇所と合わせて 1 単位**。

**LOW-APP-02 | bug | クリックによる前面移動（z 変更）がコミットされず、無関係な undo ステップに混入する**
（**解決済み**: `UIX-6`。`raised_to_front` は押下時ではなく「ドラッグが実際に
動いた最初の移動」で適用されるので、単クリックは z を触らない。ドラッグの
マウスアップは移動と raise を 1 ステップでコミットし、取り消し
（`cancel_drag`）は `NodeMoveOrigin` の位置と z を戻す。回帰 pin は
`a_node_click_raises_nothing_until_the_drag_moves`
（`crates/ravel-app/src/panels/node_editor.rs`））
`crates/ravel-app/src/panels/node_editor.rs:1744`
`raised_to_front` がマウスダウン時に表示グラフを変更する。
単なるクリックではコミットされないので refresh で元に戻る、
または次の無関係な `commit_graph` に相乗りする。
→ ドラッグが実際に動くまで raise を遅延させる。または z が変わったならマウスアップでコミット。

**LOW-CORE-05 | bug | レイヤー id 0 は保存できるが `layer.ref` からは「対象なし」になる**
（**解決済み**: `Document::validate` がレイヤー id 0 を拒否する
（`DocumentValidationError::ReservedLayerId`）。個票が推した前者の向きで、
`AssetId::UNSET` の規約と揃う。`id.rs` の `LayerId::new` は据え置き —
`composition/compile.rs:158` が Background 合成ノードの決定的 id の材料として
`LayerId::new(0)` を本番で使っている（レイヤーとしてではなくハッシュの入力として）。
回帰テストは `validate_rejects_the_reserved_layer_id`
（`crates/ravel-core/src/composition/mod.rs`）で、2 枚目以降にある 0 も拒否する）

**該当**: `crates/ravel-core/src/id.rs` の `LayerId::new` と
`crates/ravel-nodes/src/layer_ref.rs` の対象解決

`LAYER_ID_COUNTER` は 1 始まりなので `LayerId::next()` は 0 を返さないが、
`LayerId::new(0)` は公開されていて serde も 0 を受け取り、`Document::validate`
も 0 を拒否しない。つまり**手編集した `.ravprj` にはレイヤー id 0 が居られる**。

一方 `layer.ref` は 0 を必ず「対象なし」として扱う。そうせざるを得ない理由が
あって、`eval::identifier_overlay` が「静止していない識別子」に
`AssetId::UNSET`（= 0）を代入するので、評価側は 0 を未設定と読むしかない。

結果、id 0 のレイヤーは**候補ドロップダウンには出るのに参照できない**
（UX 不変条件 6「動かない制御は無効化し、無効に見せる」に触る）。

直す向きは 2 つ。`Document::validate` がレイヤー id 0 を拒否して
「0 は存在しない」を不変条件にするか、未設定の綴りを 0 と別にする
（`identifier_overlay` の代入値を変える）。前者の方が小さく、
`AssetId::UNSET` の規約（「0 は未設定のために空けてある」と
`id.rs:24` のコメントが言っている）とも揃う。

到達には手編集が要るので low。`.ravprj` v13 の `layer.ref` 移行
（`contextual-parameter-options-plan.md` の `CPO-5`）の独立レビューで見つけた。

**LOW-CORE-06 | bug | v12 以前の `layer.ref` は、移行後も出力型が `port` に追随しないまま残る**
（**解決済み**: 前者（移行パスに retype を通す）を採った。
`Document::retype_layer_ref_outputs`（`composition/layer_ref_retype.rs`）が
v13 の String 化の直後・同じ `source_version < 13` の内側で走り、
`dependent_port_updates` の答えを `set_params_and_output_types` で適用する。
コンポジションだけを歩く（型は別レイヤーの `net.out` から来るので
`Composition` が要る。平坦グラフは所有コンポジションが無く、
`dependent_port_updates` が空を返すので歩かない）。新しい型を受理しない
入力へのエッジは落ち、ノード id と前後の型を添えて `tracing::warn!` に出る
— 個票が書くとおり、そのエッジは元から壊れていた（宣言が `FRAME_BUFFER`
だから繋げただけで、評価は参照先ポートの値を渡していた）。値は書き換えず、
既に正しい型のノードと解決できない参照は触らないので冪等。回帰テストは
`crates/ravel-core/src/composition/layer_ref_retype.rs` の 6 件と、
`a_pre_v13_layer_ref_output_opens_declaring_the_port_it_references`
（`crates/ravel-project/src/lib.rs`））

**該当**: `crates/ravel-core/src/composition/layer_ref_upgrade.rs`（v12 → v13 の
型付きパス）と `crates/ravel-core/src/registry/builtin.rs` の
`dependent_port_updates`

`port` は v13 より前は自由文字列だったので、**`"frame"` 以外を打ち込んだ文書が
あり得る**。その文書の `layer.ref` ノードの出力型はテンプレート既定の
`FRAME_BUFFER` のまま保存されている（追随の機構が無かったので）。

出力型の追随は**パラメータ編集の経路にしか入っていない**
（`apply_property_change` → `dependent_port_updates` →
`set_params_and_output_types`）。v13 の移行パスは `set_params` を呼ぶので
retype しない。したがって旧文書を開くと:

- 評価は `port` の指すポートの値（例: ジオメトリ）を返す
- 宣言された出力型は `FRAME_BUFFER` のまま

という **`MED-APP-29` の症状がその 1 ノードにだけ残る**。`layer` か `port` を
一度触れば直る（そこで追随が走る）ので自己修復するが、触るまでは型が嘘をつく。

実害は小さい。旧文書は出力型が `FRAME_BUFFER` だったので**ジオメトリ入力へ
繋いだエッジは存在し得ない**（型が受理しない）ため、絵が変わる経路は無く、
繋ごうとしたときに拒否されるだけ。

**UI は評価器と同じ答えを出す。** `layer_ref_out_node` は数値綴りを読まない
（`static_text_identifier`）ので、この状態のノードでは `port` の候補も空、
出力型の追随も起きない。つまり「UI では解決しているのに評価が落ちる」には
ならず、単に**移行が済むまで何も動かない**。

直す向きは 2 つ。移行パスに retype を通す（`Document` 側で走るので
`Composition` は見えるが、ロード時にエッジを落とす可能性を持ち込む）か、
ロード後に「型が嘘をついているノード」を警告として出す
（`ColorMigrationReport` の前例）。**世に出た `.ravprj` が少ないうちは後者でも
足りる。**

`CPO-3` / `CPO-4` の独立レビューで見つけた残余。
