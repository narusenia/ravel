---
paths:
  - "crates/ravel-app/**/*.rs"
  - "crates/ravel-ui/**/*.rs"
  - "crates/ravel-dock/**/*.rs"
  - "crates/ravel-widgets/**/*.rs"
---

# UX invariants

Twelve rules the UI must not break. They exist because the open panel bugs
are not twelve unrelated defects — they are the **same few rules broken in
different places** (`docs/implementation/ui-component-layer-plan.md`).

Each rule below carries **what it looks like when broken**, because an
abstract principle cannot be checked. If a diff cannot be argued against the
symptom, the rule is not being applied.

`ravel-review` walks this file. `scripts/lint-patterns.sh` mechanically
enforces rule 12 only; the other eleven need a reader.

## 1. Selection ownership

**A panel never writes a selection it does not own.**

*Broken:* you select a layer, click in the Viewer, and the Properties panel
switches to something you did not pick — or goes empty. The Viewer owns the
canvas selection, not the properties target, so writing
`SelectedPropertiesTarget` unconditionally destroys another panel's state
(`MED-APP-05`, `MED-APP-06`).

Owning a selection means: this panel is the only writer, and it clears the
selection only for reasons of its own. A panel that needs another panel's
selection to change asks through a command, never by assignment.

## 2. Selection lifetime

**A selection dies when its context does.**

*Broken:* you click a Timeline layer header, press Delete expecting the
layer to go, and a keyframe you selected minutes ago disappears instead.
The keyframe selection outlived the row it belonged to and kept intercepting
Delete (`MED-APP-04`).

Changing what is being edited — the composition, the layer, the network, the
open document — invalidates every selection scoped inside it. Clear on the
transition, not lazily on the next read.

## 3. One action, one undo step

**A gesture that changed nothing records nothing.**

*Broken:* you drag a Timeline bar and drop it where it started; Cmd+Z now
does nothing visible, and the change you actually wanted to undo is one step
further back (`MED-APP-07`). Or you click a layer to bring it forward, and
the z change rides along inside an unrelated undo step, so undoing that step
silently reorders your layers (`LOW-APP-02`).

Compare before committing. A no-op gesture is not an edit. Conversely, one
user-visible action is exactly one step: a port rename that also drops
parameters and edges is one snapshot, not three.

## 4. A drag can always be abandoned

**Escape, a lost button, or a vanished target ends a drag cleanly.**

*Broken:* you start dragging a node, the window loses the mouse button (an
alt-tab, a system dialog, a trackpad hiccup), and the node keeps following
the cursor with no button held. Escape does nothing (`MED-APP-03`).

A drag handler checks `pressed_button` every move, ends on Escape, and ends
when the thing being dragged stops existing. Ending means the state is
restored, not left half-applied — and a drag that was abandoned records no
undo step (rule 3).

## 5. Operable at any width

**A panel declares its minimum width and decides what gets cut.**

*Broken:* you narrow the Properties panel and a Vector row's Y field slides
out past the right edge, unreachable, with no scrollbar and no indication
that it is there (`MED-UI-07`).

Below the declared minimum the panel may scroll or elide, but never hide a
control without saying so. Which side gets cut is a decision, not an
accident: labels elide, values do not.

## 6. A visible control does something

**If it cannot act, it is disabled and looks disabled.**

*Broken:* the curve editor shows a Fit button; you press it and nothing
happens, because vertical zoom was never implemented (`MED-APP-17`). You
cannot tell whether the button is broken or your curve is already fitted.

An unimplemented control is worse than a missing one: it costs the user a
guess every time. Disable it, and when the reason is not obvious, say the
reason.

## 7. A value's meaning is visible in its editor

**Component names, units, and type belong on screen.**

*Broken:* a `Channel4` parameter always renders as a colour picker, so a
4-component value that is not a colour cannot be read or typed
(`MED-APP-19`). A Vector field shows two unlabelled numbers with no way to
tell X from Y and no link toggle (`MED-APP-20`). A keyframe row names its
components from arity alone, so a 3-vector's rows read `0 / 1 / 2` whether
it is a position or an RGB (`MED-APP-30`). A layer reference is a number
scrubber, so pointing it at a different layer cannot change the output type
(`MED-APP-29`).

The editor is chosen from what the value *means*, not from how many floats
it has.

## 8. A setting that can be written takes effect

**Nothing persists a preference it does not apply.**

Locale, appearance, and the cache budget are wired
(`app_settings::install` → `apply`). `auto_save_interval_seconds`,
`proxy_resolution`, and the OCIO settings are **not** — they round-trip
through `settings.toml` and change nothing (`MED-APP-10`).

*Broken:* the settings dialog offers a control, the value survives a
restart, and the behaviour never changes. The user concludes the feature is
broken, which is correct.

A field with no consumer is either wired or removed. Not both.

## 9. A document change invalidates what was derived from it

**Caches keyed by anything but content go stale.**

*Broken:* you File ▸ Open a different project and the Media Bin shows the
previous project's thumbnails, because the cache is keyed by asset id and
ids repeat across documents (`MED-APP-08`).

Key derived state by the thing it was derived from — document revision,
content hash, resolved path — never by an index or an id that another
document can reuse. When in doubt, drop the cache on the transition.

## 10. Reachable and operable from the keyboard

**Tab reaches it, Enter or Space acts, Escape leaves, and focus is visible.**

Ravel has no Tab order today: `tab_index`, `tab_stop`, and
`accessibility_id` appear zero times. The 115 `focus_handle` sites route
panel commands; none of them make a control keyboard-reachable.

*Broken:* a user who cannot or does not want to use the mouse cannot press
a button. Or Tab moves focus somewhere invisible, so they cannot tell what
Enter will do.

Every interactive part declares `tab_index` / `tab_stop`, activates on both
Enter and Space **through the same path as a click** (two paths means one
of them rots), dismisses on Escape, and paints a focus indicator that is
distinct from hover. Screen-reader narration is out of scope; an
`accessibility_id` is not.

## 11. Motion is for state feedback only

**Values and layout change instantly.**

Animate only `hover`, `focus`, `press`, and `disabled` appearance, 100–180 ms.
The exceptions are named, and this list is exhaustive:

| May animate | Why |
|---|---|
| A toast arriving and leaving | It appears where nothing was; the eye needs to find it |
| A drop target lighting up | It *is* the drag feedback |
| hover / focus / press / disabled appearance | Rule 11 itself |

*Broken:* a panel slides open while the playhead is running, and the
animation competes with the frame budget — the user reads it as the
application lagging. Or a scrubbed value eases toward its target, so the
number on screen is not the number in the document.

In a tool where scrub and playback run at 60 fps, animating layout is
indistinguishable from being slow, and animating a value is a correctness
bug.

## 12. Colour, spacing, and type come from tokens

**No literals outside the token module.**

*Broken:* 24 hard-coded `rgb(0x…)` sites and five different row heights
(22 / 24 / 26 / 28 / 20) that differ by implementation accident rather than
by meaning. A theme change misses whichever sites were written by hand.

The row heights are now two — 20 for a row that shows one value, 24 for a row
in a list — and each panel's constant is held to its token by a test
(`the_row_height_is_the_token`). A panel that needs a third height is
describing a meaning the two steps do not carry; say which, rather than
writing a number.

`scripts/lint-patterns.sh` enforces this one (`colour-literal`). A justified
exception goes in `scripts/lint-patterns.allow` with its reason — node
category colours and Viewer guide colours are the expected ones, because they
are functional colours rather than palette steps.

**Functional colour gets one named palette module per subsystem, and the
exception is that module** — not the file that paints from it. A mark whose job
is to be told apart from the marks beside it (a port's data type, a Viewer
overlay, a curve series, a load band) has no palette step to come from, because
the palette has no opinion about which two things must never look alike. The
four that exist are `node_editor/port_colors.rs`,
`node_editor/load_colors.rs`, `panels/viewer/overlay_colors.rs` and
`panels/timeline_colors.rs`. Keeping them gathered is what keeps the exception
countable: a literal written beside its painter earns a violation, not an allow
entry.

**Dimensions are a reader's job, not the lint's.** `px()` is as often a
coordinate transform as a spacing step, so the script does not match it.

## How to use this file

Reviewing a UI diff: for each rule, ask whether the diff *could* break it,
and if so argue against the symptom rather than the principle. Most diffs
touch two or three rules.

Fixing one of the cited issues: the issue text in `issues/` is authoritative
for the defect. This file says which rule it violates, which is what tells
you whether the fix generalises — a guard in the one caller the ticket names
leaves every sibling caller broken.
