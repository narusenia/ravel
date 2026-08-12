# GPU デバイス喪失からの復旧計画

> **Status**: Planned — `HIGH-33`
>
> この文書は設計ゲート用の実装計画である。今回の変更では `crates/` 配下の
> コードを書かない。実装時はこの計画を単位ごとに分割し、各単位の完了条件を
> 満たしてから次へ進む。

## 問題

### 起動時に作った GPU がセッションの最後まで残る

`GpuContext` は `ravel-gpu` の `GpuContextInner` に `wgpu::Instance`、adapter、
device、queue、dispatch 状態、staging 状態をまとめて `Arc` で保持する
（`crates/ravel-gpu/src/device.rs:143-167`）。現状、この context は loss epoch や
喪失状態を持たず、デバイス喪失を通知する callback も登録していない。

アプリ起動時の所有関係は次のとおりである。

```text
RavelWorkspace::new
  └─ ProjectState::new_on_host_gpu
       ├─ ProjectState.gpu: Option<GpuContext>
       └─ ProjectState.eval: EvalService
            └─ worker thread
                 ├─ Evaluator
                 └─ GpuEvalHooks
                      ├─ GpuContext
                      ├─ ShaderManager
                      └─ Arc<Mutex<TexturePool>>
                           └─ GpuContext
```

- `RavelWorkspace::new` は `host_gpu_context` を一度だけ呼び、取得した context を
  `ProjectState::new_on_host_gpu` に渡す（`crates/ravel-app/src/workspace.rs:997-1005`）。
- wgpu-backed な Linux / FreeBSD / Windows では `host_gpu_context` が
  `Window::gpu_context_full()` の instance / adapter / device / queue を unpack し、
  `ravel_gpu::interop::context_from_wgpu` で Ravel の context に包む
  （`crates/ravel-app/src/workspace.rs:911-937`）。macOS ではこの関数は常に
  `None` を返す（同 `:983-988`）ため、Ravel が自前で `GpuContext::new_blocking()`
  を実行する。
- `ProjectState::new_on_host_gpu` は `GpuEvalHooks::with_budget(gpu_ctx.clone(), ...)`
  を作って `EvalService` を spawn し、同じ `GpuContext` を `ProjectState.gpu` に
  保持する（`crates/ravel-app/src/project_state.rs:477-523`）。`Evaluator` は
  `EvalService` の worker thread 内に作られるため、`ProjectState` が直接持つのは
  `EvalService` のハンドルである。
- `GpuEvalHooks` は `GpuContext`、`ShaderManager`、`Arc<Mutex<TexturePool>>` を
  worker 側に保持する（`crates/ravel-nodes/src/eval_hooks.rs:35-48`）。GPU processor
  は `GpuEvalHooks` の `gpu` / `shaders` / `pool` を通るので、単に
  `ProjectState.gpu` を差し替えても既存 `Evaluator` の processor は差し替わらない。
- `TexturePool` 自身も `GpuContext` を強く保持する（`crates/ravel-gpu/src/texture_pool.rs:260-275`）。
  `GpuFrameBuffer` は context の clone と pool の `Weak` を保持し、最後の clone の
  `Drop` で pool へ `PooledTexture` を返す（`crates/ravel-gpu/src/frame.rs:30-63`、
  `:45-53`）。したがって Viewer の `ViewerFrame`、`ViewerPanel.gpu_frame`、
  frame cache、GPUI の completion callback は worker より長く死んだ device の
  texture を保持し得る。
- `CacheBudget` は `ProjectState` が一つ作り、`EvalService` の frame cache と
  `GpuEvalHooks` の texture / media cache が同じ `SharedCacheBudget` を共有する
  （`crates/ravel-app/src/project_state.rs:478-490`、`crates/ravel-nodes/src/eval_hooks.rs:66-80`）。
  GPU epoch だけを差し替えて会計を初期化する入口はない。

### wgpu の loss と GPUI の recovery がずれる

Cargo は GPUI を `gpui-ce-ravel` の rev `27cbf19d9c516d785b2cc07a94e65a40c66cc6a8`
から取得している（`Cargo.toml:41-42`、`Cargo.lock:2703-2705`）。この worktree には
`crates/gpui_wgpu/` は存在しないため、以下の GPUI 事実は Cargo の git checkout
（`~/.cargo/git/checkouts/gpui-ce-ravel-*/27cbf19`）を読んで確認したものとして扱う。

フォークの `gpui_wgpu::WgpuContext::new_with_options` は、GPUI が作った
`wgpu::Device` に `set_device_lost_callback` を一度登録し、`device_lost` の
`AtomicBool` を立てる（依存 checkout `crates/gpui_wgpu/src/wgpu_context.rs:9-17,92-117`）。
`WgpuRenderer::recover` は loss を検出すると renderer resources と共有 context を
落とし、新しい instance / surface / `WgpuContext` を作って共有 context を差し替える
（同 checkout `crates/gpui_wgpu/src/wgpu_renderer.rs:2575-2666`）。従って Ravel が
起動時に採用した `wgpu::Device` は、GPUI が復旧した後には死んだ device である。

wgpu の callback は「複数の購読者へ broadcast」ではない。`Device::set_device_lost_callback`
は内部の `device_lost_closure` を置き換え、wgpu-core も `.replace(...)` を使う
（依存 checkout `wgpu/src/api/device.rs:590-596`、`wgpu-core/src/device/global.rs:1995-2007`）。
したがって採用デバイスに Ravel がもう一つ callback を登録するのは、GPUI の callback
を保持したまま二重登録する方法ではなく、既存 callback を上書きする方法である。

プラットフォーム別の入口は異なる。

| 経路 | 現在のデバイス | loss を知る入口 | recovery 後の問題 |
|---|---|---|---|
| Linux / FreeBSD | GPUI の wgpu device を起動時に採用 | `Window::gpu_device_lost()` は GPUI 側の loss flag を返す。`gpu_context_full()` は recovery 中 `None`、完了後は新しい context を返す | loss flag は recovery 後に `false` へ戻るので、device identity / epoch を別に追わないと交換を見失う |
| Windows | `gpui_windows` の wgpu/DX12 device を起動時に採用 | 同じ GPUI API。現行 fork の Windows platform window は full context と loss flag を返す | 上記と同じ。既存の `AdoptedHostDevice` は起動時の device しか記憶しない |
| macOS | GPUI の Metal-native device と同じ native device を照合した、Ravel 自前の wgpu logical device | Ravel 自前 context の loss は wgpu callback で検出できる。GPUI Metal renderer の loss / device recreation は、現行 `gpui_macos` と `Window` の public API には `gpu_device_lost()` が無く、**現時点では知れない** | Ravel の wgpu logical device だけが復旧しても、GPUI Metal 側が同じ native device / queue を使い続ける保証は未確認。GPUI fork に明示的な status / epoch 口を足すか、確実な fallback に留める必要がある |

### 現在の緩和の範囲

`workspace.rs::host_device_unchanged` は、Global に保存した起動時の
`AdoptedHostDevice` と、各 paint 前に `Window::gpu_context_full()` から取り直した
現在の `Arc<wgpu::Device>` を比較する（`crates/ravel-app/src/workspace.rs:940-981`）。
Viewer 側も loss flag が `true` の間は surface を使わず、flag が `false` に戻った
後も identity が違えば surface を使わない
（`crates/ravel-app/src/panels/viewer.rs:1776-1802`）。これで次のことは防げる。

- GPUI が新しい device で復旧した直後に、死んだ device の texture を現在の surface
  へ渡すこと
- loss 中に surface へ submit すること
- 複数 window の renderer と採用 device が一致しない場合の直接共有

しかし、これは surface paint を止めて CPU fallback を要求するだけである。既存の
`GpuEvalHooks`、`Evaluator`、`TexturePool`、`ProjectState.gpu` は死んだ context を
握り続けるため、復旧後に GPU 評価が再開しない。また `gpu_device_lost()` は recovery
後に `false` へ戻るため、identity / epoch が無ければ再構築のトリガーにはならない。

## 目標アーキテクチャ

### デバイス喪失を epoch 付きの一級状態にする

`ravel-gpu` に wgpu の型を含まない device state の公開語彙を一つ置く。状態は次だけ
を表す。

- `epoch`: 一度でも device を交換したことを表す単調増加値
- `lost`: この epoch の device が死んでいるか
- loss reason / message は backend の enum をそのまま出さず、Ravel 独自の診断値へ
  写像する

**`Healthy` / `Lost` / `Rebuilding` / `Ready` の 4 状態は作らない。** 観測側が知り
たいのは「自分が握っている epoch は現在の epoch か」と「その device は死んでいるか」
の 2 つだけで、`Rebuilding` と `Ready` はこの 2 つから導ける。再構築中であることを
知る必要があるのは coordinator 自身であり、coordinator は自分が再構築中であることを
呼び出し文脈から知っている。状態を増やすと、それを更新し損ねる経路が増えるだけである。

`GpuContext` は state を `Arc` で共有し、context clone、`GpuEvalHooks`、pool、
`GpuFrameBuffer` が同じ epoch を観測できるようにする。public API は
`GpuContext::device_state()`、`GpuContext::epoch()` などの抽象型だけを返す。
`wgpu::Device`、`wgpu::Queue`、`wgpu::Instance` を `ravel-gpu` の通常の公開 API に
出さない。device-sharing の `context_from_wgpu` / `wgpu_instance` と native handle
は既存どおり `ravel_gpu::interop` だけを例外とし、呼び出し元は `ravel-gpu` と
GPUI host の `ravel-app` に限定する。

### 検出と再構築の責任

検出と再構築を分ける。

1. Ravel が自前で `Device` を作った context（通常の macOS の Ravel wgpu context、
   headless で作る contextを含む）は、`GpuContext` の生成時に Ravel 自身の
   `set_device_lost_callback` を登録する。callback は wgpu 型を外へ渡さず、共有
   state を `Lost` にするだけにする。`Destroyed` は明示的な teardown と区別し、
   recovery 要求にしない。
2. GPUI の device を採用した Linux / FreeBSD / Windows では、Ravel が callback を
   追加登録しない。追加登録は GPUI callback を置き換えるためである。`ravel-app`
   の recovery coordinator が `Window::gpu_device_lost()` を loss signal として
   読み、`gpu_context_full()` の `None → Some` と device identity の変化を epoch
   交換として扱う。必要なら fork に「callback を上書きせず loss epoch を読む口」を
   追加するが、Ravel 側から `set_device_lost_callback` を採用 device に呼ばない。
3. `ProjectState` が session の GPU recovery coordinator になる。各 window の paint
   は safety guard のまま side effect を行わず、loss / identity mismatch を defer
   された coordinator 通知へ渡す。coordinator は通知に載った epoch を自分の現在の
   epoch と比べ、一致するものだけを受理する。同じ epoch の複数 window の通知は
   2 つ目以降が古い epoch になるため、recovery が二重に始まらない。
4. GPUI の recovery が完了して `gpu_context_full()` が新しい device を返すまで、
   wgpu surface capability は無効にする。新しい full context を `interop` で
   `GpuContext` に包み、Ravel の評価 worker を新 epoch で作り直す。作り直しの完了後
   にだけ `viewer_surface_enabled` を戻し、最初の frame を新 context で評価する。

macOS については、Ravel 自前 wgpu context の callback を recovery の入口にできる一方、
GPUI Metal-native renderer の loss / recreation を現行 API だけで確実に知ることは
できない。この計画は macOS を**安全側で確定**させる。自前 device が死んだら zero-copy
surface を無効化して CPU fallback に落ち、GPUI Metal 側の loss は検出しないと明記する。
fork へ native loss status / epoch の口を足す話は別 issue に切り出し、この計画の単位が
その可否に依存しないようにする。検出できない状態を検出済みとして扱わない。

### worker は生かしたまま context を差し替えず、epoch ごとに作り直す

`GpuEvalHooks` の `gpu`、`ShaderManager`、`TexturePool` と各 GPU processor の
参照は worker thread 内の `Evaluator` に閉じている。既存の generic な
`EvalService` は `EvalWorkerHooks` を spawn 時に受け取り、context 差し替え API を
持たない。従って worker を生かしたまま `ProjectState.gpu` だけを交換する案は採らない。

採る経路は以下である。

```text
loss detected
    │
    ├─ mark old epoch Lost; stop new GPU submit / surface paint
    ├─ publish blank and discard old ViewerFrame GPU lease
    ├─ cancel old interactive and export work
    ├─ drop the old EvalService's channels; join its worker off the UI thread
    │    └─ old Evaluator / GpuEvalHooks / TexturePool are dropped
    ├─ obtain the recovered host context (or create a new owned context)
    ├─ create new GpuEvalHooks + TexturePool + EvalService
    ├─ issue one Structural request for the current Document
    └─ publish only the new epoch's frame; re-enable surface capability
```

停止に新しい協調プロトコルは足さない。現行 `Drop` は channel を閉じて worker handle を
捨てるだけだが、**channel を閉じること自体が既に停止指示である**（worker は現在の
evaluation を終えたところで recv に失敗して抜ける）。足りないのは「抜けたことを知る」
手段だけなので、`EvalService` に `JoinHandle` を取り出す 1 メソッド
（`shutdown(self) -> Option<JoinHandle<()>>` 相当）を足し、join は background thread で
行って完了を coordinator へ返す。`Drop` は UI thread を塞がない現行の振る舞いを保つ
（`crates/ravel-core/src/runtime/eval_service.rs:941-950`）。新 worker は old worker の
終了と old GPU resource の破棄を確認した後にだけ作る。

`EvalService::cancel_pending` は generation を進めるだけで worker の現在の evaluation を
止めない（同 `:924-939`）ため、recovery は最悪 1 評価分待つ。playback 1 フレーム分の
遅れは recovery の体感（GPUI 側の renderer 再構築が先に入る）に埋まるので、評価途中で
抜けるための cancellation token は入れない。

**stale update は既存の generation fence で弾き、`ViewerUpdate` に epoch を足さない。**
`ProjectState` は `update.generation <= self.published_generation` の update を既に捨てる
（`crates/ravel-app/src/project_state.rs:1580`）。ただし新しい `EvalService` の generation
は 0 から始まる（`crates/ravel-core/src/runtime/eval_service.rs:882`）ので、**素朴に
差し替えると新 epoch の frame が全部この fence に落ちて Viewer が更新されなくなる。**
差し替え時に次の 2 つを同じ値に揃える。

- 新 `EvalService` の generation の初期値 = old service の `latest_generation()`
- `ProjectState.published_generation` = 同じ値

こうすると、old worker が channel に残した in-flight update は generation が必ずこの値
以下なので既存 fence が捨て、新 epoch の最初の request は `+1` されて必ず通る。
`cancel_pending` の意味論を epoch 境界へ持ち越すだけであり、新しい token は要らない。
そのため `EvalService` の spawn 側に generation の初期値を渡す口が要る。

`RenderService` が遅延生成する `RenderQueue` も同じ `ProjectState.gpu` から別の
`GpuEvalHooks` を作る（`crates/ravel-app/src/export.rs:366-400`）。loss 時は進行中の
export を cancel し、old queue を新 epoch へ持ち越さない。再開は明示的な再 submit
または recovery 後に安全な queue を作る経路に限定する。

### cache budget と進行中 frame

`SharedCacheBudget` は device の所有物ではなく session の会計 authority なので、
device recovery で新しい budget を作ったり、使用量を直接ゼロへ戻したりしない。
old `EvalService` の frame cache、old `Evaluator` の node cache、`GpuEvalHooks` の
media cache と pool を drop して、それぞれの `Reservation` / texture lease の drop
で同じ budget へ返す。old worker の終了前に new worker を起動すると一時的に二つの
GPU cache が同じ会計へぶら下がるため、順序は old worker stop → old cache/resource
drop → new worker construction とする。

復旧開始時には current `ViewerFrame` を blank（または recovery 表示）へ置き、
`ViewerPanel.gpu_frame` が old `GpuFrameBuffer` を保持し続けないようにする。old
completion callback が device loss で呼ばれない可能性も考慮し、new pool と old pool
を同じ allocation にしない。

### pool lease の順序制約

`PooledHandle::Drop` は `Weak<Mutex<TexturePool>>` を upgrade できれば pool へ返却し、
できなければ texture をそのまま破棄する。device epoch の切り替えでは、次の順序を
不変条件とする。

1. old epoch を `Lost` にし、surface paint と新しい old-epoch frame の publish を止める。
2. Viewer の Global、panel、frame cache、old worker の送信待ち結果を old epoch として
   破棄する。GPUI completion callback が保持する clone は、呼ばれるまで old epoch の
   lease として残る。
3. old `EvalService` が終了した後、old `GpuEvalHooks` と `Arc<Mutex<TexturePool>>`
   を drop する。**old pool を捨てる前に old lease を新 pool へ返してはならない**。
4. old pool が無くなった後に遅れて `GpuFrameBuffer` が drop しても、`Weak` upgrade が
   失敗し、死んだ texture は pool に戻らず破棄される。これが dead texture の new
   device への混入を防ぐ。
5. new context / new pool / new worker を作り、new epoch の frame だけを publish する。

つまり、通常 frame の ZC-4 completion は「GPUI が読み終わるまで pool に返さない」
ために必要だが、loss teardown では completion を待って old pool を生かし続けること
を recovery の前提にしない。死んだ device の callback が永遠に来ない場合でも、pool
を先に切り離せば late Drop は harmless になる。pool を in-place で context 差し替え
する案はこの不変条件を破るため採らない。

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| GPULOSS-1 | `ravel-gpu` の device state（`epoch` + `lost`）と、**Ravel が自前で作る wgpu `Device` への loss callback 登録**を 1 単位で入れる。理由を抽象診断値へ写像し、`Destroyed` を実際の loss と区別し、headless から state 遷移を注入できるようにする。通常の public API に wgpu 型を出さず、`interop` だけを device-sharing の境界として維持する | ZC-8, GPUBK-9 |
| GPULOSS-2 | `EvalService` / `ProjectState` の epoch-aware worker lifecycle を実装する。`JoinHandle` を取り出す shutdown、UI 外 join、generation の epoch 間継承、`GpuEvalHooks` / `Evaluator` / `TexturePool` の再生成、同一 `SharedCacheBudget` の会計維持を行う | GPULOSS-1 |
| GPULOSS-3 | Linux / FreeBSD / Windows の採用 GPUI wgpu device の recovery coordinator を実装する。`gpu_device_lost()` の polling と `gpu_context_full()` の `None → Some` / identity change を使い、GPUI callback を上書きせず、recovery 後の新 context を `interop::context_from_wgpu` で再採用する | GPULOSS-1, GPULOSS-2 |
| GPULOSS-4 | macOS を安全側で確定させる。Ravel 自前 wgpu context の callback で `lost` を観測したら zero-copy surface を無効化して CPU fallback に留め、GPUI Metal renderer 側の loss / recreation は**検出しないと明記する**。fork へ native status / epoch の口を足す調査は別 issue に切り出し、この単位の完了条件にしない | GPULOSS-1, GPULOSS-2 |
| GPULOSS-5 | window lifecycle、export queue、Viewer lease、テストと実機確認を仕上げる。2 枚目の window、detached close / reopen、main window の再作成、device loss 中の描画停止と recovery 後の新 frame を検証し、ZC-4 / ZC-8 の未達条件と HIGH-33 の現在地を更新できる状態にする | GPULOSS-2, GPULOSS-3, GPULOSS-4 |

GPULOSS-1 が state と callback を 1 単位に抱えるのは、state 型だけを先に入れても
producer も consumer も無く、単調増加を確かめる unit test 以外に検証するものが無い
ためである。macOS の fork API 調査を GPULOSS-4 から外したのは、fork へ口を足せるか
という未解決の問いに GPULOSS-5 まで含む全体が依存するのを避けるためである。

## 単位ごとの完了条件

### GPULOSS-1

- `GpuContext` clone、`GpuEvalHooks`、`TexturePool`、`GpuFrameBuffer` から同じ
  abstract state（`epoch` と `lost`）を観測できる。
- `ravel-gpu` の通常の public API に `wgpu::Device` / `Queue` / `Instance` が現れず、
  `scripts/lint-patterns.sh` の `gpu-facade-wgpu` と `gpu-device-sharing` が通る。
- `lost` の立ち上がりと epoch の単調増加が headless の純粋な unit test で確認できる。
  状態は 2 つの値だけで、中間 phase を表す enum を追加しない。
- 自前 context の loss callback が UI / worker を直接触らず、atomic / channel を通じて
  state だけを更新する。
- `Destroyed`、通常の device loss、callback が複数回呼ばれるケースを headless injection
  test で区別できる。
- adapter が無い headless 環境でも、callback/state と lease order のテストは skip せず
  通る。実 GPU が必要な context initialization test は既存方針どおり skip を許す。
- 未確認の backend-specific recovery を抽象 API の保証として書かない。
- デバイス喪失をユーザーへ 1 度だけ通知し、このセッションでは GPU 評価が復帰しないため
  アプリの再起動を促す。

### GPULOSS-2

- old worker の現在の評価を UI thread の join で待たない。停止指示は channel の close の
  ままとし、cancellation token を新設しない。
- stop 完了前に new worker を作らず、old worker が送った update / frame が new epoch の
  `ViewerFrame` を上書きしない。
- 差し替え後の最初の request が `published_generation` の fence に落ちない。new service の
  generation 初期値と `published_generation` が old service の `latest_generation()` で
  揃うことを headless test で確認する（揃えないと新 epoch の frame が全部捨てられる）。
- old GPU frame cache / evaluator cache / hooks cache の drop 後も
  `SharedCacheBudget.stats()` の使用量が新 epoch に持ち越すべき reservation だけに
  戻る。budget の reset や二重 authority を導入しない。
- 同一 Document、同一 playhead の recovery 後に Structural request が一度発行され、
  新 context の frame が publish されることを headless worker test で確認する。
- 実際の device loss は headless で再現しないため、ここで証明するのは lifecycle、
  ordering、stale result rejection、会計である。

### GPULOSS-3

- GPUI が採用 device に持つ callback を Ravel が上書きしない。採用 device への
  `set_device_lost_callback` の呼び出しが無いことを pattern / code review で確認する。
- loss 中は surface capability が false になり、recovery 前に old texture を paint しない。
- GPUI recovery 後に取り直した full context の device identity と Ravel の新 context が
  一致し、old `AdoptedHostDevice` を永久に参照しない。
- Linux / FreeBSD / Windows の cfg、`interop` downcast、recovery 中の `None`、recovery
  後の `Some` を `cargo check` / targeted test で確認する。実機の zero-copy は手動確認で
  別途証明する。

### GPULOSS-4

- Ravel 自前 wgpu device の `lost` で zero-copy surface が無効化され、CPU fallback で
  描画が続く。これを「GPU 自動復旧の成功」として数えない。
- 現行 `gpui_macos` に `gpu_device_lost()` 相当が無いこと、したがって GPUI Metal
  queue / renderer 側の loss を Ravel が検出しないことを、コードのコメントと
  `docs/ui-impl-status.md` の双方から判別できる。fork へ口を足す可否は別 issue で扱い、
  この単位では調査しない。
- macOS の native handle は `ravel_gpu::interop` の外へ出ず、UI が native pointer を
  保持しない。

### GPULOSS-5

- 次の自動テストが通る。
  - old pool を drop した後に遅れて `GpuFrameBuffer` / completion clone が drop しても、
    dead texture が new pool に戻らない。
  - Viewer の Global / panel / frame cache の old lease が recovery で破棄される。
  - old epoch update が new epoch の frame を上書きしない。
  - second window と detached close / reopen の capability 判定が session の GPU context
    を別々に作らず、device mismatch ならその window だけ CPU fallback になる。
  - RenderService の old `RenderQueue` が cancel / drop され、new epoch の queue だけが
    new context を使う。
- 実機の手動確認は保有環境に合わせて次のように分ける。**無い環境で確認したことに
  しない。**

  | 環境 | 実 device loss | 確認できること |
  |---|---|---|
  | Windows 実機（RTX 3080 / DX12） | できる。`Win+Ctrl+Shift+B` の driver restart、必要なら `TdrDelay` を短くする | 採用経路の loss → 再採用の全経路 |
  | macOS 実機 | 意図的な loss の手段が無い。自前 device の `lost` は debug 注入で立てる | zero-copy 無効化と CPU fallback への遷移（GPULOSS-4） |
  | Parallels の Linux | 仮想 GPU に driver reset の手段が無い | cfg のビルドと起動、CPU fallback の描画。実 loss は未実施と記録する |
  | FreeBSD | **該当機を持たない** | cfg のコンパイルのみ。手動確認は実施しない |

  共通手順は、1) Viewer を表示したまま再生・scrub、2) 上表の手段で loss を発生させる、
  3) loss 中の surface 停止と panic 無しをログで確認、4) GPUI の recovery 完了後に
  context identity / epoch が更新されることを確認、5) 新しい frame が表示され、旧 frame
  の色・順序・pool lease が混ざらないことを確認、6) cache stats が増え続けないことを
  確認、とする。破壊的な kernel panic やデータ消去を手順に採用しない。
- window lifecycle は別に、a) main + detached の 2 枚、b) detached を閉じて開き直す、
  c) main window を閉じて新しい session / main window を開く、を確認する。a) と b) は
  同じ session / current context を共有し、c) は old session の pool / worker / leases
  が先に消えることを確認する。
- headless で device loss 自体を再現したとは報告しない。headless の成功範囲は state,
  lifecycle, stale update, budget, pool order とする。macOS Metal-native loss を発生・
  検出できる具体的な test hook は現時点で未確認であり、GPULOSS-4 が「検出しない」と
  決めた範囲をそのまま手動報告へ残す。

## やらないこと / 見送る選択肢

- **採用 device への二重 callback 登録**: wgpu-core の実装は callback を replace する
  ため GPUI の recovery callback を壊す。採用経路は GPUI の loss API / epoch を読む。
- **生きた worker 内で `GpuContext` だけを差し替える**: `EvalService` の generic worker、
  `Evaluator` の processor 登録、`GpuEvalHooks` の shader / pool が同じ context を捕獲
  している。in-flight request、frame cache、lease を安全に一括交換できず、停止と再生成
  の境界が曖昧になるため採らない。
- **同じ `TexturePool` を new context へ再利用する**: idle texture、dispatch bind group、
  `PooledTexture` は old device に属する。pool は epoch ごとに捨て、新 pool を作る。
- **GPU loss を CPU fallback だけで「解決」とする**: 現在の mitigation が surface crash を
  防ぐだけで GPU pipeline は死んだままという HIGH-33 の核心を残す。CPU fallback は
  recovery 中の安全弁としてのみ使う。
- **`ravel-gpu` の public API に wgpu 型や native pointer を追加する**: facade の保証と
  `gpu-facade-wgpu` / `gpu-native-handle-escape` の制約に反する。既存 `interop` の例外を
  loss API に広げない。
- **GPUI macOS renderer を全面的に wgpu 化する**: Metal-native renderer と native
  interop の既存方針を捨てる大きな別計画であり、loss を検出できる API が無いことの
  確認なしに採用しない。
- **中間 phase を持つ state enum**: `Rebuilding` / `Ready` は `epoch` と `lost` から
  導けるうえ、更新し損ねる経路を増やす。observer が要るのは「自分の epoch が現在か」と
  「死んでいるか」の 2 つだけである。
- **`ViewerUpdate` への epoch フィールド追加**: 既存の generation fence を epoch 境界で
  引き継げば同じ保証が得られる。新しい token を持ち回すと fence が 2 系統になる。
- **評価途中で抜けるための cancellation token**: channel の close で worker は現在の
  評価を終えたところで抜ける。recovery が最悪 1 評価分待つのは GPUI 側の renderer
  再構築時間に埋まる。
- **fork へ macOS native loss status を足す作業をこの計画に含める**: 可否が未確認の
  上流作業に全単位が依存する。別 issue にする。
- **headless で実機 loss を再現したことにする**: wgpu adapter が無い環境でも状態機械
  と ownership order はテストできるが、driver reset、GPUI recovery、Metal renderer
  recreation の実証には実機が必要である。

## ロードマップ上の位置づけ

フェーズ I「GPU バックエンドの内製化と拡張点」に置く。`GPUBK-9` / `ZC-8` が確定した
「GPUI と評価が同じ device に乗る」前提の後段であり、デバイスを選ぶ単位ではなく、
その device の交換を全評価 pipeline と window lifecycle に伝播する単位だからである。
`ZC-4` / `ZC-8` の完了条件に残る「デバイス喪失・ウィンドウ再作成で破綻しない」を
引き受け、GPULOSS-1〜5 が完了するまでは zero-copy の recovery 条件を未達として扱う。
