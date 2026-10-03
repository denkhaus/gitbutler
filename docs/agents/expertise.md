# Expertise: Mulch (`mulch` / `ml` CLI)

Durable, reusable know-how for this repo lives in Mulch — a structured expertise
layer in `.mulch/`, driven by the `mulch` CLI (alias `ml`). Mulch is NOT the
issue tracker (that is Seeds, see `issue-tracker.md`) and NOT the domain
glossary (that is `domain.md`): it holds transferable lessons — conventions,
patterns, failures with resolutions, decisions with rationales.

## Layout

- `.mulch/mulch.config.yaml` — configured domains
- `.mulch/expertise/<domain>.jsonl` — one file per domain, one record per line

## Read before working in an area

- `mulch prime [domain]` — the priming prompt for a domain (or the whole corpus)
- `mulch search "<query>"` — cross-domain search
- `mulch query [domain]` — list records in a domain
- `mulch status` — what exists, per domain

## Record when something non-obvious is learned

- `mulch add <domain>` — create a domain
- `mulch record <domain> --type <convention|pattern|failure|decision|reference|guide> --name "..." --content "..." --files a,b --tags x,y`
- `mulch record <domain> --type failure --description "..." --resolution "..." --evidence-seeds <gitbutler-id>` — link the record to the seed that produced it (Mulch takes Seeds ids natively via `--evidence-seeds`)
- `mulch outcome <domain> <id> --outcome-status success|failure|partial` — confirm or refute a record after using it
- `mulch validate` / `mulch audit` / `mulch sync` — schema check, corpus health, commit `.mulch/`

## Relationship to Seeds

A seed body carries **evidence links**; mulch record ids and `--evidence-seeds`
links are the durable form of that evidence. When a skill wants to cite prior
experience for a decision, cite the mulch record; when it wants to know what is
still open, ask Seeds.

## State in this repo

`.mulch/` exists but has no domains yet (`mulch status` reports "No domains
configured"). Domains are created lazily by the first `mulch record` (or
explicitly with `mulch add <domain>`) — do not scaffold domains upfront.
