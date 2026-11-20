# LLM Wiki — Design

**Date:** 2026-11-10
**Status:** approved
**Project:** World of Tanks 0.6.5 game server (greenfield repo)

## Intent

Add a persistent, LLM-maintained knowledge base to the project: the agent compiles new sources into a growing, interlinked markdown wiki instead of re-reading raw material on every question. The human curates sources and asks questions; the agent does the summarizing, cross-referencing, and bookkeeping.

## Goals

- `raw/` holds immutable sources (articles, documentation, data dumps, notes); the agent reads them and never edits them.
- `wiki/` is agent-owned: an `overview.md` synthesis, a `sources/` page per source, `concepts/` pages, `entities/` pages, and `analyses/` for filed answers and comparisons.
- `wiki/index.md` catalogs every page; `wiki/log.md` is an append-only chronological record.
- The schema — ownership, page conventions, and the ingest / query / lint workflows — lives in `AGENTS.md`, so every future session is a disciplined wiki maintainer.
- The wiki is Obsidian-compatible: the vault root is `wiki/`, pages use `[[wikilinks]]`, and the graph shows knowledge only.

## Non-goals

- No embeddings, RAG infrastructure, or search CLI (qmd or custom) yet — `index.md` is sufficient at this scale.
- No slide decks, charts, or generated presentations.
- No separate repository or second agent schema file.
- The wiki does not replace `GLOSSARY.md` / `docs/adr/` (decisions and vocabulary) or `.scratch/` (task tracker).

## Constraints

- `raw/` is read-only for the agent: no edits, moves, or deletions; a source that needs correcting is replaced by the human and re-ingested.
- Wiki prose is Ukrainian; technical terms stay in English (`BigWorld`, `opcode`, `GLOSSARY.md` terms).
- Every factual claim on a wiki page cites a repo-relative raw path (`raw/<path>`); contradictions are flagged in a blockquote rather than silently overwritten.
- Every ingest, query, and lint pass appends to `wiki/log.md`; every page is listed in `wiki/index.md`.
- Decisions and domain vocabulary stay in `GLOSSARY.md` / `docs/adr/`; wiki pages link to them instead of duplicating.
- The wiki operations are plain workflows in `AGENTS.md`, not skills; they must not be routed through the skill table.
- No git repository exists yet; the wiki is plain markdown so `git init` can version it later.

## Approaches

1. **In this repo: `raw/` + `wiki/` at the root (chosen).** The wiki stays coupled to the same git history, agent schema, and Obsidian vault conventions as the server code; one `AGENTS.md` governs both. `raw/` is the only thing the agent cannot touch.
2. **Separate repository or folder.** Breaks the knowledge ↔ project coupling; a second schema and a second context to maintain, and the agent would have to leave the workspace to use it.
3. **Nested under `docs/wiki/`.** No benefit over the root; it hides the wiki from vault conventions and makes cross-references to `raw/` noisier.

There is one sensible way, so no further alternatives were carried forward.

## Decisions

- Folder layout: root `raw/` and `wiki/` with `index.md`, `log.md`, `overview.md`, and `sources/`, `concepts/`, `entities/`, `analyses/`.
- `raw/README.md` documents how to add sources, supported formats, and the no-secrets / large-files rules.
- `wiki/index.md` starts with grouped empty sections; `wiki/log.md` starts with an `init` entry; `wiki/overview.md` starts as a placeholder synthesis.
- Schema section `## LLM Wiki` in `AGENTS.md` after `## Project`, with subsections Files, Page conventions, Ingest, Query, Lint, Boundaries.
- `research` output may be dropped into `raw/` and ingested after human review; it is a candidate source, not the wiki's source of truth.

## Left to the builder

- Exact wording of the schema and the starter pages.
- Category folders are created with `.gitkeep` so the layout is locked before the first ingest.
- Search tooling is deferred until the index stops scaling (~100 sources).
