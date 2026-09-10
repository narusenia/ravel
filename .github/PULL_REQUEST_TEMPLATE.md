<!--
日本語で書いて構いません。見出しは英語のままにしておいてください（レビュー
する側が節を探す目印なので）。各節のコメント内のヒントは、残しても消しても
かまいません。どちらでもレンダリングされません。

The headings are the point of this template: they ask for what a reviewer
cannot reconstruct from the diff. Delete a section only if it genuinely does
not apply, and say so rather than leaving it blank.
-->

## Why now

<!--
なぜこの変更を今やるのか。roadmap のフェーズ、backlog の単位 ID、issue の
番号など、根拠になるものを指してください。

The grounds: a roadmap phase, a backlog unit, an issue, or a bug report.
-->

## What changed

<!--
何を変えたか。ファイルの列挙ではなく、挙動と構造の変化を。

Behaviour and structure, not a file list — the diff already has the files.
-->

## Design decisions

<!--
**この節が一番重要です。** 分岐があったところで何を選び、なぜ選ばなかった
方を捨てたか。「そう書いてあるから」ではなく理由を。

The decisions and the reasons. Where there was a fork in the road, which way
you went and what the other way would have cost. This is the section a
reviewer cannot write for you.
-->

## Why existing behaviour is unchanged

<!--
既存の挙動が変わらないと考える根拠。無改変のゴールデン / 既存テストが
そのまま通っていること、公開 API を触っていないこと、など。挙動を意図的に
変えたなら、それを明示してください。

The grounds: untouched golden files, existing tests passing unmodified, no
public API moved. If you did change behaviour on purpose, say so here
instead — an unannounced behaviour change is the expensive kind.
-->

## Verification

<!--
実際に回したもの。`mise run check` の結果、テスト本数、`docs:check`、
実機で確かめたなら何をどう確かめたか。

What you actually ran, and what it said.
-->

- [ ] `mise run check` passes (fmt + pattern lint + clippy `-D warnings` + tests)
- [ ] `mise run docs:check` passes, if this touches documentation
- [ ] A regression test exists **and fails when the fix is reverted**
- [ ] No production dependency added (or it was agreed beforehand)

## Remaining limitations

<!--
残した制約、意図的にやらなかったこと、この PR では閉じない既知の穴。
「無し」でも構いませんが、空欄のままにはしないでください。

What you deliberately left, and why. "None" is an acceptable answer; blank
is not.
-->
