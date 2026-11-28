# Agent instructions

Several agents push to this repo at the same time. Don't let your push undo someone else's work.

- Rebase onto the latest `origin/master` right before you open, update or merge a PR. Never merge a branch that was cut from an older master without rebasing it first.
- Before merging, run `git fetch origin && git diff --stat origin/master...HEAD` and read the file list. Every changed file should belong to your task. If a file you didn't touch is in the list, or the diff deletes or rewrites code another PR just added, your branch is stale. Rebase and recheck.
- Resolve rebase conflicts by keeping both sides' intent, not by taking your copy of a file wholesale.
- Never force-push `master`, and never push a branch to `master` without checking the diff as described above.
- If you find that earlier work was lost, restore it from the original commit in a separate PR, and say which PR overwrote it.
