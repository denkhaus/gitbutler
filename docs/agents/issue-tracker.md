# Issue tracker: Seeds (`seeds` CLI)

Issues and specs for this fork live in Seeds — git-native issue tracking in
`.seeds/`, driven by the `seeds` CLI. Not GitHub Issues: upstream
(`gitbutlerapp/gitbutler`) issues and PRs are NOT our tracker. Our tracked work
is local, in `.seeds/`, and seed ids carry the prefix `gitbutler-`.

## Conventions

- **Create**: `seeds create --title "..." --type <task|bug|feature|epic> --priority <0-4|P0-P4> --desc "..."` (0/P0 = critical, 2 = medium, 4 = backlog).
- **Labels**: `--labels a,b` on create; `seeds update <id> --add-label ... --remove-label ... --set-labels ...` later. This fork's fork-specific work uses the `fork` / `fork-request` / `multi-agent` labels.
- **Read one**: `seeds show <id> --format json` — the supported read path; never parse `.seeds/issues.jsonl` by hand.
- **List / queue**: `seeds list --format json --limit 200` (the default limit of 50 truncates silently); `seeds ready` for open, unblocked work; `seeds search "<keyword>"` for full text (AND-strict — one keyword per query).
- **Claim**: `seeds update <id> --status in_progress`.
- **Amend the body**: `seeds update <id> --description "<full body>"` — this REPLACES the body wholesale, so re-emit it complete. There are no comment threads.
- **Close**: `seeds close <id> [<id>...]`. Commit the store with `seeds sync`.
- **Dependencies**: `seeds dep add <issue> <depends-on>` / `dep remove` / `dep list`; `seeds ready` and `seeds blocked` respect them.

## Ownership

Filers file UNASSIGNED: never pass `--assignee` when creating a seed.
`assignee` is the operator's ownership switch, not a default.

## When a skill says "publish to the issue tracker"

`seeds create --title "..." --type <task|bug|feature>` with a body carrying
context, acceptance criteria, and evidence (run ids, mulch record ids).

## When a skill says "fetch the relevant ticket"

`seeds show <id> --format json` — the user passes the id, or the skill finds it
via `seeds search`.

## Wayfinding operations (for `/wayfinder`)

The **map** is one epic seed; **child tickets** are seeds blocked on it.

- **Map**: `seeds create --type epic --labels wayfinder:map --title "..."`, body holding Notes / Decisions-so-far / Fog.
- **Child ticket**: `seeds create --type task --labels wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`), first body line `Part of: <map-id>`, then `seeds dep add <child> <map>`.
- **Frontier**: the map's children that are open, unblocked, and not `in_progress`; first in creation order wins.
- **Claim**: `seeds update <id> --status in_progress` — the session's first write.
- **Resolve**: append the answer to the seed body, `seeds close <id>`, then append a context pointer to the map's Decisions-so-far.
