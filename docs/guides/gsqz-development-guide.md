# gsqz Development Guide

Technical internals for developers and agents working in the gsqz codebase.

## Architecture Overview

```
CLI (main.rs, clap)
  → Config::load (layered: built-in → global → project → CLI override)
  → Execute shell command (sh -c / cmd /C)
  → Capture stdout + stderr
  → Strip ANSI escape codes
  → [Optional] Fetch daemon config overrides
  → Compressor::new(config) → compile pipeline regexes
  → compress(command, raw_output)
    → min_length check → excluded check → pipeline match (first wins) or fallback
    → apply steps sequentially (filter → group → dedup → truncate)
    → max_lines cap → empty check → savings threshold check
  → [Optional] Report savings to daemon
  → Print result with compression banner
  → Exit 0 (always)
```

### Why Exit 0?

gsqz always exits with code 0 regardless of the subprocess exit code. This is intentional — Claude Code (and other AI tools) interpret non-zero exit codes as errors and frame the output accordingly. Since gsqz wraps commands whose output the LLM needs to read as content (not as an error), the exit code must be 0. The LLM reads pass/fail from the content itself.

## Config System

**File:** `src/config.rs`

### Layered Configuration

Configs are merged in priority order (later layers override earlier):

| Layer | Path | Purpose |
|-------|------|---------|
| 1. Built-in | Compiled into binary (`config.yaml`) | Sensible defaults, 20+ pipelines |
| 2. Global | `~/.gobby/gsqz.yaml` or `$XDG_CONFIG_HOME/gsqz/config.yaml` | User-wide overrides |
| 3. Project | `.gobby/gsqz.yaml` or `.gsqz.yaml` | Repo-specific pipelines |
| 4. CLI | `--config <PATH>` | One-off override |

### Merge Semantics

- **Pipelines**: Overlay replaces by name, adds new ones
- **Settings**: Overlay wins if value differs from built-in default
- **Excluded commands**: Additive (union of all layers)
- **Fallback steps**: Overlay replaces entirely

### Data Structures

```rust
pub struct Config {
    pub settings: Settings,
    pub pipelines: BTreeMap<String, Pipeline>,
    pub fallback: Fallback,
    pub excluded_commands: Vec<String>,
}

pub struct Settings {
    pub min_output_length: usize,       // default: 1000 chars
    pub max_compressed_lines: usize,    // default: 100 lines
    pub daemon_url: Option<String>,
}

pub struct Pipeline {
    pub match_pattern: String,          // regex matched against command string
    pub steps: Vec<Step>,               // ordered compression steps
}
```

### Step Deserialization

The `Step` enum uses a custom `Deserialize` implementation (serde `Visitor` pattern) because each YAML step is a map with a single key:

```yaml
steps:
  - filter_lines:
      patterns: ['^\s*$', '^On branch ']
  - group_lines:
      mode: git_status
  - truncate:
      head: 20
      tail: 10
  - dedup: {}
```

The visitor extracts the key name (`filter_lines`, `group_lines`, `truncate`, `dedup`) and dispatches to the appropriate struct. Invalid step names produce a clear `unknown_variant` error.

## Compressor

**File:** `src/compressor.rs`

### Initialization

`Compressor::new(config)` compiles all pipeline `match_pattern` regexes and excluded command patterns at construction time (not per-compress call). Invalid regexes are silently skipped via `filter_map`.

### Compression Flow

```
compress(command, output)
│
├─ output.len() < min_output_length?
│  → return passthrough (don't compress tiny output)
│
├─ command matches excluded_commands?
│  → return excluded
│
├─ match command against pipelines (first regex match wins)
│  ├─ matched: use pipeline.steps, strategy = pipeline.name
│  └─ no match: use fallback.steps, strategy = "fallback"
│
├─ apply_steps(lines, steps) → sequential step execution
│
├─ lines.len() > max_compressed_lines?
│  → cap: keep first 60% (head) + omission marker + last 40% (tail)
│
├─ output empty after compression?
│  → return passthrough (don't hide content from LLM)
│
├─ compressed_chars >= original_chars * 95%?
│  → return passthrough (not enough savings to justify)
│
└─ return CompressionResult { compressed, strategy_name, ... }
```

### Key Thresholds

| Threshold | Value | Rationale |
|-----------|-------|-----------|
| Min output length | 1000 chars | Don't bother compressing tiny outputs |
| Min savings | 5% | If compressed is ≥95% of original, the overhead isn't worth it |
| Max compressed lines | 100 | Hard cap after all pipeline steps |
| Max lines head/tail ratio | 60/40 | Captures beginning detail + end summary |

### First-Match-Wins

Pipelines are stored as a `Vec`, not a `Map`. The first pipeline whose regex matches the command string is used. This means order matters — more specific regexes should come before general ones in the config (e.g., `cargo test` before `cargo build`).

## Step Types (Primitives)

### filter_lines

**File:** `src/primitives/filter.rs`

Removes lines matching any pattern. Opposite of grep (exclude semantics, not include).

```rust
pub fn filter_lines(lines: Vec<String>, patterns: &[String]) -> Vec<String>
```

- Compiles each pattern as a regex
- Invalid regexes are silently skipped
- Lines matching ANY pattern are removed
- Remaining lines are returned

**Example**: Remove blank lines and git metadata:
```yaml
- filter_lines:
    patterns: ['^\s*$', '^On branch ', '^Your branch is']
```

### group_lines

**File:** `src/primitives/group.rs`

Aggregates lines by mode-specific logic. 9 grouping modes available:

```rust
pub fn group_lines(lines: Vec<String>, mode: &str) -> Vec<String>
```

#### git_status

Parses git status codes (`M`, `A`, `D`, `??`, etc.), groups by status, shows counts:
```
Modified (23):
  src/main.rs
  src/lib.rs
  [... and 21 more]
```
Max 20 files per status group.

#### git_diff

Smart diff compression:
- **Lock files** (Cargo.lock, package-lock.json, etc.): collapsed to `[lock] file (+N, -N)`
- **Generated files** (.min.js, .js.map): collapsed to `[generated] file (+N, -N)`
- **Binary files**: collapsed to `[binary] file (changed)`
- **Normal files**: kept but capped at 40 lines per file (20 head + marker + 20 tail)

Preserves preamble (lines before first `diff --git` header).

#### pytest_failures

Extracts failure sections from pytest output:
- `FAILURES` section
- `warnings summary` section
- `short test summary` section
- Final summary line (passed/failed counts)

Removes all passing test output, collection messages, and metadata.

#### test_failures (generic)

Generic test output compression. Looks for failure markers (`^FAIL`, `^ERROR`, `(?i)failures?:`) and extracts from first failure to end. If no failures found, outputs `"All tests passed.\n"`.

#### lint_by_rule

Groups lint violations by rule name/code. Detects three patterns:
- Colon-prefixed codes: `E401`, `W601`
- Bracket-suffixed rules: `[rule-name]`
- Double-space-indented identifiers

Shows count + first 5 occurrences per rule.

#### by_extension

Groups lines by file extension (last word's extension). Useful for `ls`/`tree` output. Shows first 10 files per extension, sorted by count descending.

#### by_directory

Groups lines by directory path (everything before last `/`). Shows first 10 items per directory.

#### by_file

Groups grep/ripgrep-style output (`file:line: content`) by file path. Shows first 5 matches per file.

#### errors_warnings

Partitions lines into errors (`(?i)\berror\b`), warnings (`(?i)\bwarn(?:ing)?\b`), and other. Shows first 20 errors, first 10 warnings, and last 3 other lines (usually summary).

### dedup

**File:** `src/primitives/dedup.rs`

Collapses consecutive near-identical lines using number normalization.

```rust
pub fn dedup(lines: Vec<String>) -> Vec<String>
```

**Algorithm**: Replace all digit sequences with `N`, compare normalized forms. Consecutive lines with the same normalized form are collapsed:

```
error at line 42    → normalized: "error at line N"
error at line 99    → normalized: "error at line N"  (same → collapse)
error at line 7     → normalized: "error at line N"  (same → collapse)
```

Output:
```
error at line 42
  [repeated 3 times]
```

Only collapses **consecutive** duplicates — non-adjacent identical lines are preserved. This maintains the chronological structure of logs and test output.

### truncate

**File:** `src/primitives/truncate.rs`

Two modes of truncation:

```rust
pub fn truncate(lines, head, tail, per_file_lines, file_marker) -> Vec<String>
```

#### Global Truncation (default)

When `per_file_lines == 0` or `file_marker` is empty:
- If total lines ≤ head + tail: return unchanged
- Otherwise: keep first `head` lines + `[... N lines omitted ...]` + last `tail` lines

#### Per-Section Truncation

When both `per_file_lines > 0` AND `file_marker` is set:
1. Compile `file_marker` as regex
2. Split output into sections at lines matching the marker
3. Truncate each section independently (half head, half tail)

Useful for diff output where each file section should be truncated independently:
```yaml
- truncate:
    per_file_lines: 40
    file_marker: '^diff --git'
```

## Built-in Pipelines

**File:** `config.yaml` (compiled into binary)

20+ pipelines covering common developer tools:

| Pipeline | Match | Strategy |
|----------|-------|----------|
| git-status | `\bgit\s+status\b` | filter + group(git_status) |
| git-diff | `\bgit\s+diff\b` | group(git_diff) + truncate |
| git-log | `\bgit\s+log\b` | filter + truncate |
| git-transfer | `git\s+(?:push\|pull\|fetch\|clone)` | filter(progress) + truncate |
| git-mutation | `git\s+(?:add\|commit\|stash\|tag\|branch)` | filter + truncate |
| pytest | `\b(?:pytest\|py\.test)\b` | filter(pass/meta) + group(pytest_failures) |
| cargo-test | `\bcargo\s+test\b` | filter(pass/compile) + group(test_failures) |
| generic-test | `npm\s+test\|vitest\|jest\|mocha\|go\s+test` | filter(pass) + group(test_failures) |
| python-lint | `\b(?:ruff\|mypy\|pylint)\b` | dedup + group(lint_by_rule) + truncate |
| js-lint | `\b(?:eslint\|tsc\|biome\|oxlint)\b` | dedup + group(lint_by_rule) + truncate |
| go-lint | `\b(?:golangci-lint\|staticcheck)\b` | dedup + group(lint_by_rule) + truncate |
| ls-tree | `\b(?:ls\|tree)\b` | group(by_extension) + truncate |
| find | `\bfind\b` | group(by_directory) + truncate |
| grep | `\b(?:grep\|rg\|ripgrep)\b` | group(by_file) + truncate |
| build | `cargo\s+build\|go\s+build\|make\|webpack` | filter + group(errors_warnings) + truncate |
| package-mgmt | `pip\s+install\|npm\s+install\|uv\s+pip` | filter + truncate |
| docker-list | `docker\s+(?:ps\|images)` | truncate |
| container-logs | `docker\s+logs\|kubectl\s+logs` | dedup + truncate |
| gh-cli | `gh\s+(?:pr\|issue)\s+(?:list\|view)` | filter + truncate |
| download | `\b(?:wget\|curl)\b` | filter(progress) + truncate |

### Fallback

When no pipeline matches:
```yaml
fallback:
  steps:
    - truncate: { head: 20, tail: 20 }
```

Captures the beginning (usually headers/errors) and end (usually summary) of any output.

## Daemon Integration

**File:** `src/daemon.rs` (feature-gated: `#[cfg(feature = "gobby")]`)

### Config Overrides

At startup, gsqz optionally fetches `min_output_length` and `max_compressed_lines` from the Gobby daemon:

```
GET {daemon_url}/api/config/values
→ { "output_compression": { "min_output_length": 2000, "max_compressed_lines": 150 } }
```

This allows the daemon to tune compression settings globally without updating gsqz config files.

### Savings Reporting

After compression, gsqz reports savings to the daemon:

```
POST {daemon_url}/api/admin/savings/record
{ "category": "compression", "original_chars": 10000, "actual_chars": 2500,
  "metadata": { "strategy": "git-status" } }
```

Only reports for meaningful strategies — `passthrough` and `excluded` are skipped.

### Daemon URL Resolution

1. Config file `daemon_url` with `${GOBBY_PORT}` expansion
2. `GOBBY_PORT` env var → `http://localhost:{port}`
3. Default: `http://localhost:60887`

### Error Handling

All daemon communication uses 1-second timeouts and silently ignores errors. Daemon downtime never breaks compression.

## Design Decisions

### ANSI Stripping

ANSI escape codes are stripped early (before any compression logic) using regex `\x1b\[[0-9;]*[a-zA-Z]`. This ensures all downstream steps work on plain text. Color codes waste tokens and break pattern matching.

### Combined stdout + stderr

Both output streams are captured and merged before compression. stderr is appended after stdout. This matches how developers read command output — errors appear in context with regular output.

### Regex Compilation

Pipeline regexes are compiled once in `Compressor::new()`, not per-call. Internal regexes in primitives use `LazyLock` for one-time compilation. This defers startup cost while avoiding per-compression overhead.

### Savings Threshold (95%)

If compressed output is ≥95% of the original, gsqz returns the original unchanged. The 5% minimum prevents marginal compression from adding the `[Output compressed by gsqz]` banner overhead for negligible savings.

## Use Cases

### Claude Code Shell Wrapper

gsqz is typically configured as a shell wrapper in Claude Code's settings. All Bash tool commands are piped through gsqz:

```json
{
  "permissions": {
    "bash": {
      "command_wrapper": "gsqz -- {command}"
    }
  }
}
```

This automatically compresses all command output before it enters the LLM context.

### Custom Pipelines

Add project-specific pipelines in `.gobby/gsqz.yaml`:

```yaml
pipelines:
  my-test-runner:
    match: '\bmy-test-runner\b'
    steps:
      - filter_lines:
          patterns: ['^\s*$', '^Running tests']
      - group_lines:
          mode: test_failures
      - truncate:
          head: 30
          tail: 10
```

### Debugging Compression

```bash
# See which config layers are active
gsqz --dump-config

# See compression stats
gsqz --stats -- git status

# Bypass compression (run command directly)
git status
```

### Pipeline Development

When adding a new pipeline:

1. Run the target command, capture raw output
2. Identify what's noise vs signal
3. Choose steps: filter patterns → grouping mode → dedup → truncation limits
4. Add to `config.yaml` (built-in) or project `.gobby/gsqz.yaml`
5. Test with `gsqz --stats -- <command>` to verify savings
6. Target >50% reduction for the pipeline to be worthwhile

## Testing

Each primitive has comprehensive unit tests:

- **filter.rs**: Valid/invalid patterns, empty input, all-removed edge cases
- **group.rs**: All 9 modes with realistic tool output, truncation within groups
- **dedup.rs**: Consecutive groups, mixed content, near-identical detection
- **truncate.rs**: Global and per-section modes, boundary conditions
- **config.rs**: Step deserialization, config merging, regex compilation
- **compressor.rs**: Pipeline matching, fallback, max_lines cap, savings thresholds

Run tests:
```bash
cargo test -p gobby-squeeze
```
