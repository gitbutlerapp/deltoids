# Revealing a far line in a file without syntax takes one step

## Why this case exists

A file with no syntax expands 20 lines on each side per press. Revealing
a line far from the change needs several of those levels, but the reader
asked for one thing, so it is recorded as one expansion.

## Behaviours pinned

- Revealing line 5 grows the hunk around line 50 until it shows line 5.
- One shrink returns to the 3-line default context.
