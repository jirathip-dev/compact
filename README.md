# compact

Repo history fresh start: one root before a cut, one commit per landed PR after it, archive, undo (moved out of drover)

`compact` replaces a repo's integration line with one root commit (the cut's exact tree, message
`<name>: initial (history before <date> in <owner>/<name>-history)`) and one commit per landed PR after it, each
with the old commit's tree, message, author and date. Every other branch and notes ref is carried with its files
unchanged: an open PR's commits are copied onto their true base; a branch that last met the line before the cut
is carried detached; a fully merged branch is deleted (the archive keeps it). Nothing is rebased or replayed, and
every new tip's tree is checked equal to its old one (`COMPACT_TREE`).

## Install

Needs `git`, an authenticated `gh` and, for run cards, `drover` on `PATH`.

```sh
cargo install --locked --git https://github.com/jirathip-dev/compact
```

## Use

Every call names the repo and its folders:

```sh
compact --repo owner/name --line staging --clone ~/src/name \
  [--lane [host:]path]... [--fleet <drover fleet>]... [--saved ids.tsv] [--dir <scratch>] <act>
```

- `--clone`: the fleet's clone. Its `origin` is the repo pushed to; it is reset after every push.
- `--lane`: a lane folder (`host:path` runs over `ssh`). It must be clean and pushed, and it is reset after a push.
- `--fleet`: a drover fleet on the repo. No issue may sit in `review` or `verify` (`drover status <fleet> --json`).
- `--saved`: drover's stored commit ids, one `fleet<TAB>issue<TAB>sha` per line. The dry run lists the ones to
  remap, and the push writes them as `moved.json`.
- `--dir`: the scratch folder for the bare copy `old.git`, `plan.json`, `compact-map.tsv`, `moved.json` and
  `undone` (default `~/.local/state/compact/<owner>/<name>`).

Acts:

| act | what it does |
| --- | --- |
| `--cut <date\|sha> --dry-run` | Mirror origin, write the plan and map, print old → new commits, the root, each PR commit, the open PRs to carry, the records to remap and the readiness checks. Pushes nothing. |
| `--cut <date\|sha> --thread <id>` | The same, then post the owner's run cards in that drover thread (`COMPACT_NOT_READY` when a check fails). |
| `--undo [--dry-run \| --thread <id>]` | Check every ref is still where compact put it; with `--thread`, post the undo cards. |
| `--push <tip> [--main] [--undo]` | The approved card's run. Refused `COMPACT_UNAPPROVED` outside its card. |
| `--reset` | Reset the clone and lane folders onto origin's branches (files stay). |

A date cut is the last first-parent line commit before that midnight. Refusals print `REFUSED <CODE>: why` and
exit 3.

## Run cards and drover

`compact` does not depend on drover. It uses drover's CLI only:

- **Posting.** `--thread <id>` runs `drover card post --cmd … --why … --blast … --dir <cwd> --timeout 3600 <id>`
  once for the line (`compact <same arguments> --thread <id> --push <new tip>`: every ref but `main`). When `main`
  moves, it posts a second card (`… --main --push <main's new tip>`), whose blast warns that the push may fire the
  repo's main-merge workflows. Undo posts `… --undo --push <old tip>` and `… --undo --main --push <main's old tip>`.
- **Approval.** `--push` reads `drover thread show <id> --json`. It runs only while a card with exactly its
  command is approved and has not finished, so the line's card never pushes `main`. The main card runs only after
  the line is compacted (`COMPACT_NOT_READY` before).
- **Archive.** The approved line run first pushes every old branch, tag, note and PR head (as `pull/<n>`) to the
  private `<owner>/<name>-history`. It reads that back, then adds the plan and map on its `drover-compact` branch
  (`COMPACT_ARCHIVE` if the archive is public or misses a ref). Then one atomic push moves every ref but `main`,
  each with a lease on its old value (`COMPACT_MOVED` if one moved since the plan). Open PR bodies get their new
  head ids.
- **Records.** Drover keeps its own log. After the push, `<dir>/moved.json` lists per fleet and issue the saved
  ids now in the new history (`from`, `to`, `held`); drover logs them as its `compacted` records. An undo writes
  `<dir>/undone` (the run's new tip) for drover's `compact-undone`. Which stored fields drover exports through
  `--saved` (its saved-SHA list and the CI guard over it) stays in drover.

Pause the fleet (`drover pause <fleet>`) before posting the cards.

## Develop

```sh
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

`tests/compact.rs` builds a repo with merges, why notes, an open PR, a lane and `main`. It compacts the repo
through faked `gh` and `drover`, then undoes the compact, with the real binary and real git.
