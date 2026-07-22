# Agents

Guidance for AI coding agents (Claude Code, ZCode, and others) operating in this repo.

## Project shape

Termior is a Rust + GPUI desktop app, organised as a Cargo workspace under `crates/` (~17 crates). Top-level docs live in `docs/`. The authoritative product spec is `docs/termior-spec.md` (SDD, v0.1). Read it before touching product behaviour.

## Agent skills

### Issue tracker

GitHub issues for `willmove/termior`, via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Five canonical triage roles, using the same strings as label names (`needs-triage` / `needs-info` / `ready-for-agent` / `ready-for-human` / `wontfix`). See `docs/agents/triage-labels.md`.

### Domain docs

Single-context layout — one `CONTEXT.md` at the repo root and `docs/adr/` for architectural decisions. See `docs/agents/domain.md`.
