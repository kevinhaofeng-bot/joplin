# Task 7 C1 Task 2 independent review — `d5c53f479..40786e6cd`

Verdict: **NOT PASS**, 0 Critical / 3 Important / 1 Minor. The reviewer read the original Evernote 11.32.5 find UI/keybinding modules and the Task 2 plan, then inspected the production diff and ran the focused mounted Cmd-F test (1/1 passed). This is a code/interaction gate, not a claim that the native window could not launch.

## Important

1. `ui/mod.rs:3547-3569,5177-5183,6365`: if Cmd-K's global search palette is open, Cmd-F focuses an in-note input hidden underneath the global palette's occluding backdrop. The user sees global Search while input goes to Find. Coordinate close/open and deferred focus restoration; test Cmd-K→Cmd-F→typing and the reverse route.
2. `ui/mod.rs:5325,5341-5379,5432-5444`: the fixed-width input canvas paints its full unbounded `ShapedLine` without a horizontal viewport/clip or caret reveal. Long Chinese/Latin queries can cover the count and controls; the 42px count slot and error text can also overrun. Bound query paint and layout/hit targets at 900px and a narrower editor column.
3. `ui/mod.rs:969-972,3549-3555,3598-3606,3641-3643`: the input observer defers marked IME text, but Aa toggling calls the unguarded refresh directly; repeated Cmd-F calls `TitleInput::select_all`, which clears marked state. Both paths can search/scroll from provisional Chinese composition. Put the guard at the common refresh boundary and preserve marked state until unmark; add mounted regressions.

Minor: `ui/mod.rs:5432-5436` tracks focus but lacks the `TitleInput` pointer-selection handlers used by other fields, so clicking the middle of an existing query does not position the caret or support drag selection.

The reviewer found the ordinary Cmd-F/G/Shift-G action wiring, no-active-note guard, Trash read-only query route, ephemeral editor mutation boundary, and independent Cmd-K path otherwise plausible. The three Important findings were dispatched to Terra for one bounded fix round. No code was changed by the reviewer.

## Remediation and final bounded gate — `ce2efae55`

Terra commits `eccf5fa10`, `f6a40f966`, `dec84cea1`, `3bd5c874d`, and `ce2efae55` closed the three original Important findings and review-discovered follow-ons. Cmd-K and Cmd-F hand off the visible panel and original return focus; the mounted test covers Find→Cmd-K→Cmd-F→Escape. A common marked-IME guard and close/reopen tests prevent uncommitted composition from becoming a query. The query canvas and enclosing input now have the same measured width; paint scrolls to the active selection head, including reverse Shift-selection, and the input handler reports a clipped, translated IME range. A real 10,000-match note wraps to match 10,000 and checks the actual summary geometry. The Find panel remains outside the document surface.

Independent round 3 review: **0 Critical / 0 Important; C1 Task 2 code accepted**. The reviewer independently ran both focused mounted cases (1/1 each). Remaining Minors: clicking within the query does not yet position/drag the caret; the 10,000-count UI test proves a sufficiently wide real summary box and checks the production label formatter, but cannot read painted glyph text directly. These do not block the bounded C1 MVP acceptance. This is not acceptance of physical Chinese IME, populated Release result navigation, OCR/PDF find, or all Task 7/M2.
