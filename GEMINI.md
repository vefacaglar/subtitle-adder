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

## Mandatory Semantic Versioning Policy

- **Single Source of Truth:** The project version starts at `1.0.0` and is kept synchronized across `VERSION`, `engine/Cargo.toml` (`[workspace.package]`), `apps/desktop/Cargo.toml` (`[workspace.package]`), and both `Cargo.lock` files.
- **Every AI agent change MUST increment the version** according to change size before completion (use `pwsh scripts/bump-version.ps1 <patch|minor|major>` or `bash scripts/bump-version.sh <patch|minor|major>`):
  - **Patch (`+0.0.1`, e.g. `1.0.0` -> `1.0.1`):** Bug fixes, small improvements, UI polish, refactoring, CI/script or documentation/config changes.
  - **Minor (`+0.1.0`, e.g. `1.0.1` -> `1.1.0`):** New features, new language/model/subtitle capabilities, medium-to-large enhancements.
  - **Major (`+1.0.0`):** Only for breaking architectural or protocol-overhaul releases.
- **Orchestrator & Reviewer Enforcement:** The Orchestrator must include the version bump requirement when assigning tasks, and `hydrogen-reviewer` must verify that `VERSION`, `engine/Cargo.toml`, `apps/desktop/Cargo.toml`, and `Cargo.lock` were bumped appropriately before approving.
