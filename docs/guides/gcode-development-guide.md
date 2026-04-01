# gcode Development Guide

Technical internals for developers and agents working in the gcode codebase.

## Architecture Overview

```
CLI (main.rs, clap)
  → Context::resolve (config.rs)
    → detect_project_root / resolve_db_path / resolve_services
  → Command dispatch (main.rs match)
    → commands/{search,symbols,graph,index,status,init,summary}.rs
      → search/ pipeline (FTS5 + semantic + graph → RRF)
      → index/ pipeline (walker → parser → chunker → hasher → indexer)
      → neo4j (HTTP Cypher queries)
      → db (SQLite WAL connections)
      → output (JSON/text formatting)
```

### Entry Point

`main.rs` parses CLI args via clap, resolves a `config::Context` (project root, DB path, service configs), and dispatches to the appropriate command handler. Commands that work without a project context (`init`, `projects`, `prune`) are dispatched before `Context::resolve()`.

The embedding model's Metal GPU env var (`GGML_METAL_TENSOR_ENABLE`) is set at the top of `main()` before any threads spawn — setting env vars during lazy init is UB on macOS due to concurrent reads.

On exit, `semantic::shutdown()` explicitly drops the embedding model to prevent Metal residency set assertion crashes during static destructor teardown.

## Configuration Resolution

**File:** `src/config.rs`

`Context::resolve(project_override, quiet)` orchestrates the full resolution flow:

### Project Detection

1. If `--project` is a directory path, use it directly
2. If `--project` is a name, look up in `code_indexed_projects` table (basename suffix match)
3. Otherwise, walk up from cwd looking for `.gobby/project.json` (Gobby-managed) or `.gobby/gcode.json` (standalone)
4. Fall back to VCS root markers (`.git`, `.hg`, `.svn`) or cwd

### Database Path Selection

1. Check `~/.gobby/bootstrap.yaml` for `database_path` key
2. If `.gobby/project.json` exists → `~/.gobby/gobby-hub.db` (Gobby mode, shared DB)
3. Otherwise → `~/.gobby/gobby-code-index.db` (standalone mode)

### Service Configuration

Resolution order per service (Neo4j, Qdrant):

| Priority | Source | Example |
|----------|--------|---------|
| 1 (highest) | Environment variables | `GOBBY_NEO4J_URL`, `GOBBY_QDRANT_URL` |
| 2 | `config_store` table in SQLite | `databases.neo4j.url` |
| 3 (lowest) | Hardcoded defaults | `http://localhost:8474` (Neo4j only if config_store exists) |

Config values are JSON-encoded in `config_store` — strings have surrounding quotes stripped. Secret patterns like `$secret:NAME` are resolved via `secrets.rs` (Fernet decryption using machine_id + salt, 600K PBKDF2 iterations).

### Operating Modes

| Mode | Trigger | Database | Services |
|------|---------|----------|----------|
| Standalone | `.gobby/gcode.json` only | `gobby-code-index.db` | SQLite FTS5 only |
| Gobby | `.gobby/project.json` exists | `gobby-hub.db` | SQLite + Neo4j + Qdrant |

Standalone mode has no `config_store` table, so Neo4j/Qdrant defaults are never applied. Graph commands return empty results gracefully.

## Indexing Pipeline

**Files:** `src/index/{walker,parser,chunker,hasher,indexer}.rs`

### Data Flow

```
walker::discover_files(root, excludes)
  → (ast_candidates, content_only_candidates)
    → parser::parse_file(path, project_id, root, excludes)
      → tree-sitter AST → extract_symbols + extract_imports + extract_calls
      → link_parents (nest methods in classes, build qualified names)
    → chunker::chunk_file_content(source, rel_path, project_id, lang)
      → 100-line chunks with 10-line overlap
    → hasher::file_content_hash(path)
      → SHA-256 for incremental detection
    → indexer::upsert_symbols + upsert_file + upsert_chunks
      → SQLite writes + FTS5 population
      → Qdrant vector upsert (if embeddings enabled)
      → Neo4j graph writes (DEFINES, CALLS, IMPORTS edges)
```

### File Discovery (walker.rs)

Uses the `ignore` crate (`WalkBuilder`) which respects `.gitignore`, `.git/info/exclude`, and git global config. Files are partitioned into:

- **AST candidates**: Extensions matching a tree-sitter language spec (17 languages)
- **Content-only candidates**: Extensions like `.sh`, `.sql`, `.html`, `.css` — chunked for FTS but no AST parsing

Default excludes: `node_modules`, `__pycache__`, `.git`, `.venv`, `target`, `dist`, `build`, `.next`, `coverage`, etc.

### Tree-Sitter Parsing (parser.rs)

1. **Security checks**: Path validation, symlink safety, binary detection (8KB read), secret file detection, 10MB size limit
2. **Language detection**: Extension → `languages::detect_language()` → `LanguageSpec`
3. **AST parsing**: Tree-sitter grammar per language, execute symbol/import/call queries
4. **Symbol extraction**: Captures `@name` and `@definition.KIND` from query results. Kind is parsed from the capture name (e.g., `definition.function` → kind `function`)
5. **Parent linking**: For each symbol, find the largest enclosing class/type by byte range. Nested functions in classes become `method` kind. Qualified names built as `parent_name.symbol_name`
6. **Signature**: First line of definition, truncated at 200 chars
7. **Docstring**: Python/JS/TS only — first string literal in function/class body

### Language Support (languages.rs)

17 languages with tree-sitter queries for symbol definitions, imports, and call sites:

| Tier | Languages |
|------|-----------|
| Tier 1 | Python, JavaScript, TypeScript, Go, Rust, Java, C, C++, C#, Ruby, PHP, Swift |
| Tier 2 | Dart, Elixir |
| Tier 3 | JSON, YAML, Markdown (content structure only) |

Each language has a `LanguageSpec` with three tree-sitter queries: `symbol_query`, `import_query`, `call_query`. Empty queries mean that feature is disabled for the language.

### Content Chunking (chunker.rs)

Files are split into overlapping chunks for FTS5 content search:

- **Chunk size**: 100 lines
- **Overlap**: 10 lines (step = 90)
- **1-indexed**: line_start/line_end use 1-based indexing
- Empty/whitespace-only chunks are pruned

### Incremental Indexing (indexer.rs)

1. **Hash comparison**: SHA-256 content hash per file, stored in `code_indexed_files`
2. **Stale detection**: Compare current hashes against stored hashes; files with changed hashes are re-indexed
3. **Orphan cleanup**: Files in the DB that no longer exist on disk have their data deleted from SQLite, Neo4j, and Qdrant
4. **Per-file cleanup**: Before re-indexing a file, `delete_file_data` removes old symbols from SQLite, vectors from Qdrant, and graph nodes from Neo4j

The `--full` flag skips the hash comparison and re-indexes all files, ensuring stale external index entries are cleaned up.

### UUID5 Parity

Symbol IDs are deterministic UUID5 using namespace `c0de1de0-0000-4000-8000-000000000000` and key format `{project_id}:{file_path}:{name}:{kind}:{byte_start}`. This must match the Python daemon's `Symbol.make_id()` exactly — IDs are shared between gcode (Rust) and the Gobby daemon (Python).

## Search Pipeline

**Files:** `src/search/{fts,semantic,graph_boost,rrf}.rs`, `src/commands/search.rs`

### Hybrid Search (`gcode search`)

Three sources are queried and merged via Reciprocal Rank Fusion:

```
Source 1: FTS5 (SQLite full-text search)
  → search_symbols_fts (MATCH query on code_symbols_fts)
  → fallback: search_symbols_by_name (LIKE query)

Source 2: Semantic (Qdrant vector search)
  → embed query text using nomic-embed-text-v1.5 GGUF model
  → POST to Qdrant /collections/{name}/points/search
  → returns (symbol_id, score) pairs

Source 3: Graph Boost (Neo4j)
  → find_callers + find_usages for query term
  → returns symbol IDs that are connected in the call/import graph

  ↓ All three → RRF merge → Symbol resolution → Pagination
```

### RRF Merge (rrf.rs)

Reciprocal Rank Fusion with K=60:

```
score(symbol) = Σ 1/(K + rank) for each source containing the symbol
```

- Rank 0 is best (first in source list)
- Single-source max score: 1/60 ≈ 0.0167
- Multi-source: scores are additive across sources
- Results sorted by combined score, descending
- Source attribution preserved (e.g., `["fts", "semantic"]`)

### Symbol Resolution

After RRF merge, ALL symbol IDs are resolved against SQLite before computing `total`. This ensures `total` reflects the count of actually-resolvable symbols (stale Qdrant/Neo4j entries are silently skipped). Offset/limit pagination is applied after resolution.

The `total` for hybrid search is a best-effort estimate bounded by `fetch_limit` per source — exact counts aren't feasible because RRF merges results from three different systems with deduplication.

### FTS Search (`search-text`, `search-content`)

These use dedicated `count_text`/`count_content` functions (FTS5 COUNT queries) for accurate totals, separate from the paginated result fetch.

### Pagination

All search/graph commands return a `PagedResponse` envelope:

```json
{
  "project_id": "uuid",
  "total": 47,
  "offset": 0,
  "limit": 10,
  "results": [...],
  "hint": null
}
```

- `--offset N` skips the first N results
- `--limit N` caps results per page (default: 10)
- `hint` is populated when Neo4j is unavailable (graph commands only)
- Text mode shows a pagination footer: `-- 10 of 47 results (use --offset 10 for more)`

## Database Schema

### Core Tables

**`code_symbols`** — Extracted AST symbols

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID5 (deterministic, matches Python) |
| project_id | TEXT | Project identifier |
| file_path | TEXT | Relative to project root |
| name | TEXT | Symbol name |
| qualified_name | TEXT | `parent.name` for methods |
| kind | TEXT | function, class, method, type, etc. |
| language | TEXT | python, rust, etc. |
| byte_start, byte_end | INTEGER | Byte offsets for source extraction |
| line_start, line_end | INTEGER | 1-indexed line numbers |
| signature | TEXT | First line of definition (≤200 chars) |
| docstring | TEXT | Python/JS/TS only |
| parent_symbol_id | TEXT | Enclosing class/type ID |
| content_hash | TEXT | SHA-256 of symbol source |
| summary | TEXT | LLM-generated description |
| created_at, updated_at | TEXT | Epoch seconds as string |

**`code_indexed_files`** — File index metadata

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID5(project_id:file_path) |
| project_id | TEXT | |
| file_path | TEXT | Relative path |
| language | TEXT | Detected language |
| content_hash | TEXT | SHA-256 for incremental detection |
| symbol_count | INTEGER | |
| byte_size | INTEGER | |
| indexed_at | TEXT | Epoch seconds |

**`code_content_chunks`** — File content for FTS search

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | UUID5(project_id:file_path:chunk:N) |
| project_id | TEXT | |
| file_path | TEXT | |
| chunk_index | INTEGER | Sequential per file |
| line_start, line_end | INTEGER | 1-indexed, inclusive |
| content | TEXT | Chunk text (100 lines, 10-line overlap) |
| language | TEXT | |

**`code_symbols_fts`** / **`code_content_fts`** — FTS5 virtual tables for full-text search

**`code_indexed_projects`** — Project statistics

**`config_store`** — Key-value configuration (Gobby mode only)

## Neo4j Graph Model

### Nodes

- **CodeSymbol**: `{id, name, kind, project, file, line}`
- **CodeFile**: `{path, project}`
- **CodeModule**: `{name}`

### Edges

- **DEFINES**: `CodeFile -[:DEFINES]-> CodeSymbol` (file defines symbol)
- **CALLS**: `CodeSymbol -[:CALLS {file, line}]-> CodeSymbol` (call relationship)
- **IMPORTS**: `CodeFile -[:IMPORTS]-> CodeModule` (import relationship)

### Query Patterns

- **Callers**: `MATCH (caller)-[:CALLS]->(callee {name, project}) RETURN caller`
- **Usages**: `MATCH (n)-[r]->(target {name, project}) WHERE type(r) IN ['CALLS', 'IMPORTS']`
- **Imports**: `MATCH (f:CodeFile {path, project})-[:IMPORTS]->(m:CodeModule)`
- **Blast Radius**: Variable-length path traversal with depth limit

Count queries use the same patterns with `RETURN count(...)` for accurate pagination totals.

## Qdrant Integration

**File:** `src/search/semantic.rs`

### Embedding Model

- **Model**: nomic-embed-text-v1.5 Q8_0 GGUF
- **Dimension**: 768
- **Location**: `~/.gobby/models/nomic-embed-text-v1.5.Q8_0.gguf`
- **Task prefixes**: `search_query: ` for queries, `search_document: ` for indexing
- **GPU**: Metal acceleration on macOS (automatic), optional CUDA/Vulkan/ROCm on Linux

### Vector Lifecycle

1. **Index**: For each symbol, build embed text (`qualified_name + signature + docstring[:500]`), generate embedding, upsert to Qdrant
2. **Re-index**: `delete_file_data` queries symbol IDs from SQLite before deletion, then deletes corresponding Qdrant vectors via `POST /collections/{name}/points/delete`
3. **Search**: Embed query, search Qdrant, return `(symbol_id, score)` pairs

### Collection Naming

`{collection_prefix}{project_id}` — default prefix is `code_symbols_` from Qdrant config.

## Graceful Degradation

Each external service degrades independently:

| Service | When Unavailable | Impact |
|---------|-----------------|--------|
| Neo4j | No config or connection refused | Graph commands return `[]` with hint; search loses graph boost |
| Qdrant | No URL configured | Search loses semantic source; FTS5 still works |
| GGUF model | File not found | Semantic search disabled (no embeddings to query with) |
| Daemon | Not running | `invalidate` can't notify; savings not reported |

The system always works with just SQLite — FTS5 search and outline are fully functional in standalone mode.

## Output Format

### Default (Slim)

- **Search**: `id`, `name`, `qualified_name`, `kind`, `file_path`, `line_start`, `score`, `signature`, `sources` — no `summary`
- **Outline**: `id`, `name`, `kind`, `line_start`, `line_end`, `signature` (6 fields vs 18 on full Symbol)
- **Graph**: `id`, `name`, `file_path`, `line`, `relation`, `distance`

### Verbose (`--verbose`)

- **Search**: Adds `summary` field (LLM-generated description)
- **Outline**: Returns full `Symbol` struct with all 18 fields

Fields never shown even in verbose: `content_hash`, `created_at`, `updated_at`, `byte_start`, `byte_end`. `project_id` is hoisted to the `PagedResponse` envelope.

## Use Cases

### Agent Search Workflow

1. `gcode search "auth middleware"` → ranked results with file:line, signature
2. Pick result → `gcode symbol <id>` (exact source code) or `Read file_path offset=line_start`
3. Need more? → `gcode search "auth middleware" --offset 10`

### File Survey

1. `gcode outline src/config.rs` → slim symbol list (name, kind, lines, signature)
2. Identify relevant functions → `gcode symbol <id>` for source code

### Impact Analysis

1. `gcode blast-radius "handleAuth" --depth 3` → transitive dependents
2. `gcode callers "handleAuth"` → direct call sites
3. `gcode usages "DatabasePool"` → all references (calls + imports)

### Cross-Project Queries

```bash
gcode search --project myapp "query"        # by folder name
gcode search --project /path/to/myapp "query"  # by path
```

### Full Reindex

```bash
gcode index --full    # re-process all files, clean stale vectors
gcode invalidate      # destructive reset (drops all data, re-create)
```
