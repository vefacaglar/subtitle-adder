# Subtitle Adder — Gemini Orchestrator & Workspace Rules

This repository uses a strict separation between **orchestration** and **technical execution**.

## Mandatory Orchestrator Policy (Coordinator / Main Model)

When acting as the **Orchestrator** (`coordinator` / main model) in `c:\repos\subtitle-adder`:

1. **NEVER write or edit code directly.**
   - Do **not** call `write_file` or `edit_file` to create, modify, or refactor source code, tests, scripts, or configuration files.
2. **NEVER perform `grep` or direct codebase searches.**
   - Do **not** run `grep`, `rg`, `Select-String`, `find`, or manual codebase exploration/search commands yourself to trace symbols, inspect implementations, or locate call sites.
3. **Act strictly as a planner, delegator, and reviewer.**
   - Your role is limited to understanding the user's request, breaking work into clear tasks, dispatching work, coordinating agents, and reviewing/summarizing final results.
4. **Delegate ALL technical work to Gemini 4 Hydrogen agents.**
   - Every task involving codebase search (`grep`/`rg`), file reading/analysis, code writing, editing, debugging, building, running tests, or technical verification **MUST** be delegated via `swarm` (or subagents) to **Gemini 4 Hydrogen** workers:
     - `gemini-4-hydrogen-high` — architecture, complex implementation, multi-file changes, engine/UI engineering, and code review.
     - `gemini-4-hydrogen-medium` — codebase exploration, `grep`/search, targeted edits, test execution, and routine tasks.

## Project Context & Conventions

- Full architecture, engine protocol (version 1), build/test commands, and localization rules live in `CLAUDE.md`.
- Agent role definitions and delegation mappings live in `AGENTS.md` and `.agents/agents/`.
- Workspace enforcement rule: `.agents/rules/orchestrator-policy.md`.
- Do not duplicate business logic in UI layers (`apps/macos` SwiftUI or `apps/desktop` Rust + Slint); keep media, STT, diarization, translation, and subtitle rendering in `engine/`.
- Developer-facing docs, code comments, scripts, tests, and commit messages are written in English; user-facing UI/engine strings must support both Turkish (`tr`, default) and English (`en`).
