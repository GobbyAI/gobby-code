# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

A Cargo workspace containing two Gobby CLI tools:

- **gcode** (`crates/gcode/`) — AST-aware code search, symbol navigation, and dependency graph analysis. Reads/writes the same databases as the Gobby daemon (SQLite, Neo4j, Qdrant).
- **gsqz** (`crates/gsqz/`) — YAML-configurable output compressor for LLM token optimization. Wraps shell commands and applies pattern-based compression pipelines.

## Build & Test Commands

```bash
cargo build --workspace                              # Build everything (gcode requires cmake for embeddings)
cargo build --workspace --no-default-features        # Build everything without embeddings
cargo test --workspace                               # Test everything
cargo test --workspace --no-default-features         # Test without embeddings
cargo test -p gobby-code --no-default-features       # Test gcode only
cargo test -p gobby-squeeze                          # Test gsqz only
cargo clippy --workspace --no-default-features -- -D warnings  # Lint all
cargo fmt --all --check                              # Check formatting
```

The `embeddings` feature (gcode, default: on) enables local GGUF embedding via `llama-cpp-2` and requires cmake. Metal GPU acceleration is automatically enabled on macOS via target-conditional dependencies; non-macOS platforms get CPU-only inference. CI builds that don't need embeddings use `--no-default-features`.

## Workspace Layout

```
crates/
  gcode/    — Heavy binary (tree-sitter, SQLite, Neo4j, Qdrant, opt-level=3)
  gsqz/     — Tiny binary (regex pipelines, shell wrapper, opt-level="z")
```

Release profiles are in the root `Cargo.toml` with per-package overrides. Each binary has its own optimization level.

## gcode Architecture

### Data Flow

`main.rs` parses CLI args via clap → resolves a `config::Context` (project root, DB path, service configs) → dispatches to the appropriate command handler in `commands/`.

### Core Modules

- **`config`** — Resolves runtime context: `~/.gobby/bootstrap.yaml` → SQLite `config_store` → Neo4j/Qdrant configs. Detects project root by walking up from cwd looking for `.gobby/project.json`. Resolves `$secret:NAME` patterns via `secrets`.
- **`db`** — Thin SQLite connection helpers (`open_readwrite` with WAL, `open_readonly`). All connections use 5s busy timeout.
- **`models`** — All data types: `Symbol`, `IndexedFile`, `ContentChunk`, `SearchResult`, `GraphResult`, etc.
- **`secrets`** — Fernet decryption of Gobby secrets using `~/.gobby/machine_id` + `~/.gobby/.secret_salt` for key derivation.
- **`neo4j`** — HTTP client for Neo4j Cypher queries (callers, usages, imports, blast radius).
- **`output`** — Output formatting (text vs JSON).

### `commands/` — CLI Command Handlers

Each subcommand maps to a function: `index::run`, `search::search`, `symbols::outline`, `graph::callers`, etc. Commands accept `&Context` and an output `Format`.

### `index/` — Indexing Pipeline

`walker` (file discovery via `ignore` crate) → `parser` (tree-sitter AST extraction per language) → `chunker` (content splitting for FTS) → `hasher` (SHA-256 for incremental indexing) → `indexer` (SQLite writes + FTS5 population). `languages` maps extensions to tree-sitter grammars. `security` validates paths.

### `search/` — Search Pipeline

`fts` (FTS5 symbol + content search) + `semantic` (Qdrant vector search) + `graph_boost` (Neo4j relevance boost) → `rrf` (Reciprocal Rank Fusion to merge ranked results).

### Graceful Degradation

Neo4j/Qdrant/GGUF model can each be unavailable independently. Graph commands return `[]` when Neo4j is down; search loses the corresponding boost but FTS5 always works if the project is indexed.

## gsqz Architecture

### Data Flow

CLI parses args → loads layered config → executes shell command → strips ANSI codes → optionally fetches daemon config overrides → matches command against pipeline regexes (first match wins) → applies step sequence → optionally reports savings to daemon → prints compressed output.

**Always exits with code 0** — intentional to prevent Claude Code from framing compressed output as an error.

### Core Modules

- **`config`** — Layered config system: built-in `config.yaml` → global (`~/.gobby/gsqz.yaml`) → project (`.gobby/gsqz.yaml`) → CLI override. Custom `Visitor` deserializer for the polymorphic `Step` enum.
- **`compressor`** — Orchestrator that compiles pipeline regexes, matches commands, applies steps, and enforces thresholds (min output length, max compressed lines, 95% savings threshold).
- **`daemon`** — Feature-gated (`#[cfg(feature = "gobby")]`) HTTP integration with the gobby daemon for runtime config overrides and savings reporting. All HTTP calls are fire-and-forget with 1s timeouts.
- **`primitives/`** — Four composable operations on line collections: `filter`, `group` (8 modes), `dedup`, `truncate`.

## Key Constraints

- **UUID5 parity with Python** (gcode): Symbol IDs are deterministic UUID5 using namespace `c0de1de0-0000-4000-8000-000000000000` and key format `{project_id}:{file_path}:{name}:{kind}:{byte_start}`. Must match the Python daemon's `Symbol.make_id()` exactly.
- **Config resolution order** (gcode): env vars (`GOBBY_NEO4J_URL`, etc.) → `config_store` table → hardcoded defaults.
- **Tree-sitter grammars** (gcode): Tier 1 (Python/JS/TS/Go/Rust/Java/C/C++/C#/Ruby/PHP/Swift/Kotlin), Tier 2 (Dart/Elixir), Tier 3 (JSON/YAML/Markdown). Adding a language requires a new `tree-sitter-*` dep in `crates/gcode/Cargo.toml` and a grammar entry in `index/languages`.
- **Non-destructive to Gobby databases** (gcode): Detect and skip existing Gobby-owned databases and tables. Never alter `project.json` or Gobby-managed schema.
- **Exit code 0** (gsqz): Always exit 0 regardless of subprocess exit code. The LLM reads pass/fail from content.
