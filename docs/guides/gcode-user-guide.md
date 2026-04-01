# gcode User Guide

A complete guide to using `gcode` for code search, symbol navigation, and dependency analysis.

## Getting Started

### Install

Download from [GitHub Releases](https://github.com/GobbyAI/gobby-cli/releases/latest) or build from source:

```bash
# With embeddings (macOS gets Metal GPU automatically)
cargo install gobby-code

# With GPU acceleration on Linux/Windows
cargo install gobby-code --features cuda    # NVIDIA
cargo install gobby-code --features vulkan  # Any GPU
cargo install gobby-code --features rocm    # AMD

# Without embeddings (smallest binary, FTS5 search only)
cargo install gobby-code --no-default-features
```

If you use [Gobby](https://github.com/GobbyAI/gobby), gcode is already installed.

### Initialize and Index

```bash
cd your-project
gcode init
```

`gcode init` does everything in one step:
1. Creates `.gobby/gcode.json` (project identity file)
2. Installs AI CLI skills (Claude Code, etc.) if detected
3. Indexes the entire project with tree-sitter

You'll see a progress bar while indexing:

```text
Indexing src/config.rs ████████████░░░░░░░░ 18/32
```

After init, you can search immediately.

### First Search

```bash
gcode search "handleAuth"
```

Returns matching symbols ranked by relevance — function names, class definitions, method signatures — with file paths, line numbers, and signatures. JSON output is wrapped in a pagination envelope showing `total`, `offset`, and `limit`.

## Search

gcode offers three search modes for different use cases.

### Hybrid Search (`gcode search`)

The default. Combines FTS5 text matching, semantic vector similarity, and graph relevance using Reciprocal Rank Fusion.

```bash
gcode search "database connection pool"
gcode search "auth" --limit 5
gcode search "handler" --kind function
gcode search "config" --offset 10          # Page 2 of results
```

**When to use:** General-purpose queries. Best for natural language and conceptual searches.

**Options:**
- `--limit N` — Max results (default: 10)
- `--offset N` — Skip first N results for pagination (default: 0)
- `--kind <kind>` — Filter by symbol kind: `function`, `class`, `method`, `type`, etc.

### Text Search (`gcode search-text`)

FTS5 search on symbol metadata: names, qualified names, signatures, docstrings, and summaries.

```bash
gcode search-text "parseConfig"
```

**When to use:** You know the exact name or part of a symbol name. Fastest mode.

### Content Search (`gcode search-content`)

FTS5 search across file content chunks — finds matches in comments, strings, configuration, and code bodies.

```bash
gcode search-content "TODO: refactor"
gcode search-content "GOBBY_NEO4J_URL"
```

**When to use:** Searching for string literals, comments, configuration values, or patterns that aren't symbol names.

## Symbol Retrieval

### Outline

Get the hierarchical symbol tree for a file:

```bash
gcode outline src/config.rs
```

Returns all functions, classes, methods, structs, etc. in the file with their line ranges and signatures. JSON output uses a slim format (id, name, kind, line_start, line_end, signature) — use `--verbose` for full symbol details. Much cheaper than reading the entire file.

### Symbol by ID

Fetch the exact source code of a symbol by its ID (from search or outline results):

```bash
gcode symbol "80abc77f-bdfe-5037-94a8-1ebcb753761d"
```

Returns the symbol with its full source code extracted via byte-offset read. Precise and minimal.

### Batch Retrieve

Fetch multiple symbols in one call:

```bash
gcode symbols "id1" "id2" "id3"
```

### File Tree

Get the project's file tree with symbol counts per file:

```bash
gcode tree
```

Useful for understanding project structure at a glance.

## Dependency Graph

Graph commands require Neo4j (available in Gobby mode). In standalone mode, they return empty results gracefully.

### Callers

Who calls this function?

```bash
gcode callers "handleAuth"
gcode callers "handleAuth" --limit 20
gcode callers "handleAuth" --offset 10    # Page 2
```

### Usages

All references — calls and imports:

```bash
gcode usages "DatabasePool"
```

### Imports

Show the import graph for a file:

```bash
gcode imports src/auth/middleware.ts
```

### Blast Radius

Transitive impact analysis — what breaks if this changes?

```bash
gcode blast-radius "handleAuth" --depth 3
```

Walks the call graph to find all downstream dependents up to `--depth` levels deep.

## Project Management

### Status

Check the current project's index stats:

```bash
gcode status
```

Returns file count, symbol count, last indexed time, and duration.

### List Projects

See all indexed projects across both databases:

```bash
gcode projects
```

### Cross-Project Queries

Query a different project by name or path:

```bash
# By name (matches against project directory basename)
gcode search --project myapp "query"

# By path
gcode search --project /home/user/projects/myapp "query"
```

Name resolution looks up the `code_indexed_projects` table in both `gobby-code-index.db` and `gobby-hub.db`.

### Re-indexing

Incremental re-index (only changed files):

```bash
gcode index
```

Full re-index (re-processes all files, cleans stale external index entries):

```bash
gcode index --full
```

Index specific files:

```bash
gcode index --files src/config.rs src/main.rs
```

Reset and rebuild from scratch (destructive — prompts for confirmation):

```bash
gcode invalidate
gcode index
```

In Gobby mode, `invalidate` also notifies the daemon to clean up Neo4j graph nodes and Qdrant vectors for the project. Use `--force` to skip the confirmation prompt.

## Operating Modes

### Standalone Mode

When there's no `.gobby/project.json` in the project, gcode operates independently:
- Database: `~/.gobby/gobby-code-index.db`
- Services: SQLite only (FTS5 search)
- Identity: `.gobby/gcode.json`

This is the default for projects not managed by Gobby. All indexing and FTS5 search work fully.

### Gobby Mode

When `.gobby/project.json` exists (Gobby manages the project):
- Database: `~/.gobby/gobby-hub.db` (or path from `bootstrap.yaml`)
- Services: SQLite + Neo4j + Qdrant (if configured)
- Identity: `.gobby/project.json`

Graph commands and semantic search become available in this mode.

## Configuration

gcode resolves configuration in this order:

1. **Environment variables** — `GOBBY_NEO4J_URL`, `GOBBY_NEO4J_AUTH`, `GOBBY_QDRANT_URL`, `GOBBY_PORT`
2. **config_store table** — Key-value pairs in the SQLite database (`databases.neo4j.*`, `databases.qdrant.*`)
3. **Hardcoded defaults** — Neo4j at `http://localhost:8474`, database `neo4j`

The database path itself is resolved from:
1. `~/.gobby/bootstrap.yaml` `database_path` key
2. Default based on mode (standalone vs Gobby)

The daemon URL (used by `invalidate`) is resolved from:
1. `GOBBY_PORT` environment variable (e.g. `60887`)
2. `~/.gobby/bootstrap.yaml` `daemon_port` + `bind_host` keys
3. Default: `http://localhost:60887`

## Output Formats

All commands support `--format`:

```bash
gcode search "query" --format json   # Default — structured JSON
gcode search "query" --format text   # Human-readable text
```

JSON output for search/callers/usages is wrapped in a pagination envelope:

```json
{
  "project_id": "3bf57fe7-...",
  "total": 47,
  "offset": 0,
  "limit": 10,
  "results": [...]
}
```

Text output shows a pagination hint when more results are available:

```text
-- 10 of 47 results (use --offset 10 for more)
```

### Verbose output

`--verbose` controls both GGML debug logging and output detail level:

```bash
gcode search "query" --verbose   # Include summaries in results
gcode outline src/main.rs --verbose  # Full symbol details instead of slim
```

Default output is optimized for token efficiency — slim fields, no AI-generated summaries. Use `--verbose` when you need the full picture.

Suppress warnings and progress bars with `--quiet`:

```bash
gcode index --quiet
```

## Troubleshooting

### "No gcode project found"

You haven't initialized the project yet:

```bash
gcode init
```

Or specify a project explicitly:

```bash
gcode search --project /path/to/project "query"
```

### "Project 'foo' not found"

The project name doesn't match any indexed project. Check available projects:

```bash
gcode projects
```

### Empty search results

- Run `gcode status` to verify the project is indexed
- Try `gcode search-text` for exact name matches
- Try `gcode search-content` for string/comment searches
- Run `gcode index` to pick up recently changed files

### Graph commands return empty arrays

This is expected in standalone mode (no Neo4j). In Gobby mode, check that Neo4j is running and configured:

```bash
echo $GOBBY_NEO4J_URL
gcode status
```

### Slow first index

Tree-sitter parsing is fast but scales with codebase size. Subsequent runs are incremental — only changed files are re-indexed. Large `node_modules`, `target`, `.venv` directories are excluded automatically.
