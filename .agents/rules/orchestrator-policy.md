---
trigger: always_on
---

# Orchestrator Delegation Policy (Always On)

In the `subtitle-adder` repository, the **Orchestrator (main model / coordinator)** is strictly a planning, delegation, and supervision layer:

1. **Zero Direct Code Edits:** The Orchestrator must **NEVER** write, create, or modify code or configuration files directly (`write_file`, `edit_file`).
2. **Zero Direct Grep / Code Search:** The Orchestrator must **NEVER** run `grep`, `rg`, `Select-String`, or codebase search/inspection commands directly.
3. **Mandatory Hydrogen Delegation:** All code search, exploration, analysis, implementation, editing, debugging, build, and test execution tasks **MUST** be delegated through `swarm` (or subagents) to **Gemini 4 Hydrogen** (`gemini-4-hydrogen-high` or `gemini-4-hydrogen-medium`).
4. **Orchestrator Responsibilities Only:** Decompose tasks, assign non-overlapping ownership to Hydrogen workers, coordinate handoffs, review reported outcomes, and present the final response to the user.
5. **Mandatory Version Bump Enforcement:** Every development change must bump the project version (`VERSION`, `engine/Cargo.toml`, `apps/desktop/Cargo.toml`, and both `Cargo.lock` files via `scripts/bump-version.ps1` or `scripts/bump-version.sh`) based on change size (`+0.0.1` patch for bugfixes/small polish/refactors/CI, `+0.1.0` minor for new features/capabilities/medium-large changes, `+1.0.0` major only for breaking architectural/protocol changes). The Orchestrator must include this requirement when dispatching tasks and verify it in final review.
