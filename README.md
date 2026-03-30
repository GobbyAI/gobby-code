<!-- markdownlint-disable MD033 MD041 -->
<p align="center">
  <img src="logo.png" alt="Gobby" width="160" />
</p>

<h1 align="center">gobby-cli</h1>

<p align="center">
  <strong>Rust CLI tools for AI-assisted development.</strong><br>
  Code search, symbol navigation, and output compression — all from the terminal.
</p>

<p align="center">
  <a href="https://github.com/GobbyAI/gobby-cli/actions/workflows/ci.yml"><img src="https://github.com/GobbyAI/gobby-cli/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/GobbyAI/gobby-cli/releases/latest"><img src="https://img.shields.io/github/v/release/GobbyAI/gobby-cli" alt="Release"></a>
  <a href="https://github.com/GobbyAI/gobby-cli"><img src="built-with-gobby.svg" alt="Built with Gobby"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache%202.0-blue.svg" alt="License"></a>
</p>

---

## What's Inside

This workspace contains two Gobby CLI tools:

| Binary | Package | Description |
|--------|---------|-------------|
| `gcode` | `gobby-code` | AST-aware code search, symbol navigation, and dependency graph analysis |
| `gsqz` | `gobby-squeeze` | YAML-configurable output compressor for LLM token optimization |

See [crates/gcode/README.md](crates/gcode/README.md) and [crates/gsqz/README.md](crates/gsqz/README.md) for detailed documentation on each tool.

## Install

### Pre-built binaries

Download from [GitHub Releases](https://github.com/GobbyAI/gobby-cli/releases/latest). Binaries are available for macOS (ARM/x86), Linux (x86/ARM), and Windows (x86/ARM).

### From source

```bash
# gcode (with embeddings — requires cmake)
cargo install gobby-code

# gcode (without embeddings)
cargo install gobby-code --no-default-features

# gsqz
cargo install gobby-squeeze
```

## Development

```bash
cargo build --workspace --no-default-features   # Build both tools
cargo test --workspace --no-default-features    # Test both tools
cargo clippy --workspace -- -D warnings         # Lint both tools
cargo fmt --all --check                         # Check formatting
```

## License

Apache 2.0 — see [LICENSE](LICENSE).
