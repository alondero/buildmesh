# ADR 0060: Normalize repository text to LF in Git

Status: accepted

## Context

Windows and Linux contributors run source and manifest checks over the same
files. Checkout defaults can otherwise make the file bytes depend on each
contributor's Git settings.

## Decision

`.gitattributes` normalizes detected text to LF in Git and defaults to LF in the
worktree. PowerShell and Windows batch files check out as CRLF for their
interpreters while staying LF in Git. Generated TypeScript bindings and the
harness catalog use LF to match the generator. The bindings also allow
trailing spaces during diff checks because ts-rs may emit them before field
documentation.

Git treats the HTTP fuzz seeds as binary. Line endings, invalid UTF-8, NUL bytes
and header whitespace are part of the test input.

## Alternatives and consequences

Relying on `core.autocrlf` would make checkout bytes depend on local settings.
Normalizing every file as text would risk corrupting binary fixtures and
assets. Git's content detection handles ordinary assets and explicit
attributes preserve the fuzz corpus. `npm run check:eol` verifies the index.
The line-ending tests verify the checkout attributes.

## Links

- [Development harness](../agents/development-harness.md#worktree-setup)
- [Rust conventions](../development/rust-conventions.md)
