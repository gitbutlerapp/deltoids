# Revealing a line grows the nearest hunk until it shows the line

## Why this case exists

Go to definition can land on an unchanged line of a changed file that no
hunk draws. The TUI then reveals it: the nearest hunk grows the way the
expand key grows it, as far as needed to show the line, and one shrink
folds it back.

## Behaviours pinned

- Revealing `pub fn total` (line 10) grows the `savings` hunk to the
  whole `impl`, the first level that holds line 10.
- One shrink folds the hunk back to the method, even though the reveal
  could have taken several expansion levels.
- Revealing a line a hunk already draws changes nothing.
- Revealing `pub fn unrelated` (line 19), outside the `impl`, grows the
  hunk to the whole file.
