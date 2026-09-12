# Task 7 Stage B2c independent review — `82d946daa..332e77002`

Verdict: **not accepted yet**. The reviewer confirmed the Dirty→Clean same-generation revision fence, mounted stale-packet test, actual measured 500-row scroll/last-row click, refresh Err→visible Retry→success test, and no new foreground FTS or second projection authority. The remaining findings are below. Review was read-only and did not re-run the full suite.

## Important

1. `ui/mod.rs:3674-3681`: history restore treats `Ok(false)` like success, clears the notice and does not inspect/re-schedule the latest still-pending history target. A stale completion can leave a SearchRoute destination unrestored.
2. `ui/mod.rs:5839-5849`: Retry click removes the button and says retrying without checking whether history scheduling returned `false` or active refresh is still pending. After navigation changes this may leave a permanent non-actionable retrying notice. No fault-injected Back/Forward history Retry click proof.
3. `ui/mod.rs:3442-3463`: opening Cmd-K closes toolbar More; closing restores only organization-panel visibility before refocusing the prior handle. The reviewer requests proof for another focusable entry context and its containing overlay, including backdrop dismissal; a focus handle belonging to an unmounted overlay must not be restored blindly.

## Minor and verification

- `ui/mod.rs:3563-3585,3729-3752`: test-only OS-thread gate can remain blocked if a test panics before release; use cleanup/timeout.
- `ui/tests.rs:657-713`: keyboard Down→tail visible and tail click pass; Up/actual wheel-tail not asserted separately.
- At review pin, full GPUI suite in implementer report predates the last change. Controller independently ran final-head core suite, GPUI 1294/0/1, both format checks and diff check after receiving this verdict; Release rebuild and disposable-profile smoke are separate remaining gates.

The reviewer did not find actual unpacked Evernote modules under this worktree or `/Users/kevinhao/Projects/joplin`. Controller verified the mapped raw modules at `/Users/kevinhao/Projects/joplin-reconstruction/evernote-11.32.5/main-readable/src/modules/83028__module-83028.js` and `17897__module-17897.js`; this path omission in the dispatch is a review evidence limitation, not an implementation failure.

Source: independent `/root/task7_b2c_review_sol` verdict delivered 2026-09-13. No code was changed by the reviewer.
