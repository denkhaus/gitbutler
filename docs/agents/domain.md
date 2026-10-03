# Domain Docs

How the engineering skills should consume this repo's domain documentation when
exploring the codebase. Layout: **single-context**.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root.
- **`docs/adr/`** — system-wide decisions.
- The nearest nested `AGENTS.md` (`crates/`, `apps/lite/`, `packages/ui-react/`) for code-scoped rules.

This repo is a fork of `gitbutlerapp/gitbutler` and ships its own docs; none of
the files above exist yet. If a file is missing, **proceed silently** — don't
flag its absence and don't suggest creating it upfront. `/domain-modeling` (via
`/grill-with-docs` and `/improve-codebase-architecture`) creates `CONTEXT.md` and
`docs/adr/` lazily, when terms or decisions actually get resolved.

If a `CONTEXT-MAP.md` later appears at the root, the repo has moved to
multi-context: read the map first, then the per-context `CONTEXT.md` files
relevant to the topic.

## File structure (single-context)

```
/
├── CONTEXT.md              ← created lazily by /domain-modeling
├── docs/adr/               ← created lazily by /domain-modeling
├── crates/                 Rust: but-* workspace + legacy gitbutler-* crates
├── apps/desktop            Tauri + Svelte desktop app
├── apps/lite               Electron + React app
├── apps/web                Svelte web app
├── packages/               shared TS: but-sdk, ui-react, ui-svelte, core
└── e2e/                    Playwright / WebdriverIO / blackbox
```

## Use the glossary's vocabulary

When your output names a domain concept (in a seed title, a refactor proposal, a
hypothesis, a test name), use the term as defined in `CONTEXT.md`. Don't drift to
synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal: either you're
inventing language the project doesn't use (reconsider) or there's a real gap
(note it for `/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than
silently overriding:

> _Contradicts ADR-0007 (event-sourced orders), but worth reopening because…_
