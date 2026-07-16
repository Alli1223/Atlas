## Summary

<!-- One or two sentences: what this does and why. Link the TODO.md phase. -->

## What changed

<!--
The notable moves, not a file list — the diff already lists files.
Call out anything a reviewer would otherwise have to reverse-engineer:
schema changes, migrations, new dependencies, decisions that went a
non-obvious way. If it changed an architectural decision, add or update
an ADR in docs/adr/.
-->

-

## How to test

<!-- Exact steps. Assume a clean checkout. -->

```
just dev
```

1.

## Screenshots

<!-- UI changes only. Before/after if you changed something that existed. Delete if N/A. -->

## Checklist

- [ ] Tests pass (`just test`)
- [ ] Lint clean (`just lint`) — no new clippy or eslint warnings
- [ ] Formatted (`just fmt-check`)
- [ ] Docs updated (README / ADR / inline) where behaviour changed
- [ ] `TODO.md` phase items ticked, progress table updated
- [ ] Migrations are reversible and `.sqlx/` regenerated (`just prepare`) if queries changed
- [ ] No secrets, keys, or tokens in the diff — including tests and fixtures
