# Subtitle Adder — Agent Architecture & Delegation Guide

## Operating Model

This repository enforces a strict two-tier agent workflow:

- **Orchestrator (`coordinator` / main model):**
  - Plans tasks, splits independent work into slices, assigns briefs, tracks progress, and inspects final summaries.
  - **Prohibited:** Directly editing/writing code (`write_file`, `edit_file`), running `grep`/`rg`/codebase searches, or performing hands-on debugging/implementation.
- **Workers — Gemini 4 Hydrogen (`gemini-4-hydrogen-high` / `gemini-4-hydrogen-medium`):**
  - Perform **all** codebase searches (`grep`, `rg`), file inspection, implementation, refactoring, build/test execution, and verification.
  - Must always be spawned via `swarm` (or configured subagents) with explicit model pinning to `gemini-4-hydrogen-high` or `gemini-4-hydrogen-medium`.

## Recommended Hydrogen Model Allocation

| Role | Antigravity Agent | Swarm Model | Scope & Responsibilities |
| :--- | :--- | :--- | :--- |
| **Explorer** | `hydrogen-explorer` | `gemini-4-hydrogen-medium` | Read-only codebase search, `grep`/`rg`, symbol lookup, API verification (`~/.cargo/registry`, `.build/checkouts`). Writes no code. |
| **Engineer** | `hydrogen-engineer` | `gemini-4-hydrogen-high` (or `medium` for small/focused tasks) | Code implementation, bug fixes, tests, and build verification across `engine/` (Rust), `apps/desktop` (Rust + Slint), `apps/macos` (SwiftUI/EngineKit), and `scripts/`. |
| **Reviewer** | `hydrogen-reviewer` | `gemini-4-hydrogen-high` | Read-only correctness, protocol parity (`engine/PARITY.md`, `apps/desktop/PARITY.md`), Swift 6 concurrency, layer boundaries, and test verification. |

## Domain Ownership Reference (Aligned with `.claude/agents/`)

When writing swarm briefs for Hydrogen workers, assign clear file ownership so parallel workers never edit the same files:

- **Media (`engine/crates/se-audio`)**: FFmpeg 8.1 extraction (16 kHz mono WAV), preview generation.
- **STT & Diarization (`engine/crates/se-asr`, `engine/crates/se-diarize`, `engine/crates/se-models`)**: `Transcriber` backends (`ct2rs`, `ort`), sherpa-onnx diarization, model catalog/cache.
- **Subtitles & Translation (`engine/crates/se-text`, `engine/crates/se-translate`, `engine/crates/se-cli`, `engine/crates/se-core`)**: sentence segmentation, SRT/VTT/JSON rendering, translation, CLI protocol events.
- **Desktop App (`apps/desktop/`)**: Rust + Slint (`sa-core`, `sa-enginekit`, `sa-viewmodels`, `sa-player`, `sa-app`).
- **macOS App (`apps/macos/`)**: SwiftUI (`SubtitleAdderApp`) and `EngineKit`.

## Swarm Delegation Checklist for the Orchestrator

1. Before starting any technical investigation or edit, spawn a `gemini-4-hydrogen-medium` (search/explore) or `gemini-4-hydrogen-high` (implementation/review) worker via `swarm`.
2. Specify exact file/crate ownership and verification commands in the brief (`cargo test --workspace -j 2` for `engine/`, `cargo test --workspace -j 4` for `apps/desktop/`, `swift test` for `apps/macos/`).
3. Assign and verify the mandatory semantic version bump (`VERSION`, `engine/Cargo.toml`, `apps/desktop/Cargo.toml`, and both `Cargo.lock` files via `scripts/bump-version.ps1` or `scripts/bump-version.sh`) at the end of every development task:
   - **Patch (`+0.0.1`, e.g. `1.0.0` -> `1.0.1`):** Bug fixes, small improvements, UI polish, refactoring, CI/script/doc updates.
   - **Minor (`+0.1.0`, e.g. `1.0.1` -> `1.1.0`):** New features, new languages/models/subtitle capabilities, medium-to-large enhancements.
   - **Major (`+1.0.0`):** Breaking architectural or protocol-overhaul releases only.
4. Wait for worker results via `swarm_wait`; never fall back to doing `grep` or `edit_file` directly.
