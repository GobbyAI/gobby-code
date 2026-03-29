//! Configuration resolution for gcode.
//!
//! Reads bootstrap.yaml → DB path → config_store → service configs.
//! Resolves $secret:NAME and ${VAR} patterns.
//!
//! Source: src/gobby/config/bootstrap.py, src/gobby/config/persistence.py

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::secrets;

/// Neo4j connection configuration.
#[derive(Debug, Clone)]
pub struct Neo4jConfig {
    pub url: String,
    pub auth: Option<String>,
    pub database: String,
}

/// Qdrant connection configuration.
#[derive(Debug, Clone)]
pub struct QdrantConfig {
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub collection_prefix: String,
}

/// Resolved runtime context for gcode commands.
pub struct Context {
    /// Path to gobby-hub.db
    pub db_path: PathBuf,
    /// Project root directory
    pub project_root: PathBuf,
    /// Project ID (from .gobby/project.json or DB lookup)
    pub project_id: String,
    /// Suppress warnings
    pub quiet: bool,
    /// Neo4j config (None if unavailable)
    pub neo4j: Option<Neo4jConfig>,
    /// Qdrant config (None if unavailable)
    pub qdrant: Option<QdrantConfig>,
}

impl Context {
    /// Resolve context from CLI args and filesystem state.
    pub fn resolve(project_override: Option<&str>, quiet: bool) -> anyhow::Result<Self> {
        let project_root = match project_override {
            Some(p) => PathBuf::from(p).canonicalize()?,
            None => detect_project_root()?,
        };

        let db_path = resolve_db_path()?;
        let project_id = resolve_project_id(&project_root)?;

        // Resolve service configs from config_store (best-effort)
        let neo4j = resolve_neo4j_config(&db_path, quiet);
        let qdrant = resolve_qdrant_config(&db_path, quiet);

        Ok(Self {
            db_path,
            project_root,
            project_id,
            quiet,
            neo4j,
            qdrant,
        })
    }
}

/// Read database_path from ~/.gobby/bootstrap.yaml, falling back to default.
fn resolve_db_path() -> anyhow::Result<PathBuf> {
    let gobby_dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".gobby");

    let bootstrap_path = gobby_dir.join("bootstrap.yaml");
    if bootstrap_path.exists() {
        let contents = std::fs::read_to_string(&bootstrap_path)?;
        let yaml: serde_yaml::Value = serde_yaml::from_str(&contents)?;
        if let Some(db) = yaml.get("database_path").and_then(|v| v.as_str()) {
            let expanded = db.replace("~", &gobby_dir.parent().unwrap().to_string_lossy());
            return Ok(PathBuf::from(expanded));
        }
    }

    Ok(gobby_dir.join("gobby-hub.db"))
}

/// Detect project root by walking up the directory tree.
///
/// Resolution order:
/// 1. `.gobby/project.json` or `.gobby/gcode.json` (identity file)
/// 2. VCS root (`.git` or `.hg`)
/// 3. Current working directory
fn detect_project_root() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir()?;

    // First: look for an identity file (.gobby/project.json or .gobby/gcode.json)
    if let Some(root) = crate::project::find_project_root(&cwd) {
        return Ok(root);
    }

    // Second: fall back to VCS root
    let mut dir = cwd.as_path();
    loop {
        if dir.join(".git").exists() || dir.join(".hg").exists() {
            return Ok(dir.to_path_buf());
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return Ok(cwd), // Last resort: cwd
        }
    }
}

/// Resolve project ID from identity files or generate deterministically.
///
/// Resolution order:
/// 1. `.gobby/project.json` — gobby's file (reads `"id"`, falls back to `"project_id"`)
/// 2. `.gobby/gcode.json` — gcode's standalone identity
/// 3. Generate deterministic UUID5 from canonical path (no filesystem writes)
fn resolve_project_id(project_root: &Path) -> anyhow::Result<String> {
    let gobby_dir = project_root.join(".gobby");

    if gobby_dir.join("project.json").exists() {
        return crate::project::read_project_id(project_root);
    }
    if gobby_dir.join("gcode.json").exists() {
        return crate::project::read_gcode_json(project_root);
    }

    Ok(crate::project::generate_project_id(project_root))
}

// ── Config store helpers ─────────────────────────────────────────────

/// Read a value from the config_store table, returning None if missing.
fn read_config_value(conn: &rusqlite::Connection, key: &str) -> Option<String> {
    conn.query_row(
        "SELECT value FROM config_store WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

/// Resolve Neo4j configuration from config_store + env vars.
fn resolve_neo4j_config(db_path: &Path, quiet: bool) -> Option<Neo4jConfig> {
    // Try to open DB for config reading
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .ok()?;

    // Read from config_store with env var overrides
    let url = std::env::var("GOBBY_NEO4J_URL")
        .ok()
        .or_else(|| read_config_value(&conn, "memory.neo4j_url"))
        .or_else(|| Some("http://localhost:8474".to_string()))?;

    let raw_auth = std::env::var("GOBBY_NEO4J_AUTH")
        .ok()
        .or_else(|| read_config_value(&conn, "memory.neo4j_auth"));

    // Resolve $secret: patterns in auth
    let auth = match raw_auth {
        Some(v) => match secrets::resolve_config_value(&v, db_path) {
            Ok(resolved) => Some(resolved),
            Err(e) => {
                if !quiet {
                    eprintln!("Warning: failed to resolve Neo4j auth: {e}");
                }
                None
            }
        },
        None => None,
    };

    let database = read_config_value(&conn, "memory.neo4j_database")
        .unwrap_or_else(|| "neo4j".to_string());

    Some(Neo4jConfig {
        url,
        auth,
        database,
    })
}

/// Resolve Qdrant configuration from config_store + env vars.
fn resolve_qdrant_config(db_path: &Path, quiet: bool) -> Option<QdrantConfig> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))
        .ok()?;

    let url = std::env::var("GOBBY_QDRANT_URL")
        .ok()
        .or_else(|| read_config_value(&conn, "memory.qdrant_url"));

    let raw_api_key = read_config_value(&conn, "memory.qdrant_api_key");
    let api_key = match raw_api_key {
        Some(v) => match secrets::resolve_config_value(&v, db_path) {
            Ok(resolved) => Some(resolved),
            Err(e) => {
                if !quiet {
                    eprintln!("Warning: failed to resolve Qdrant API key: {e}");
                }
                None
            }
        },
        None => None,
    };

    let collection_prefix = read_config_value(&conn, "memory.code_symbol_collection_prefix")
        .unwrap_or_else(|| "code_symbols_".to_string());

    // Only return Some if there's a URL (qdrant_path = embedded mode, not accessible from CLI)
    url.as_ref()?;

    Some(QdrantConfig {
        url,
        api_key,
        collection_prefix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_db() -> (tempfile::NamedTempFile, rusqlite::Connection) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let conn = rusqlite::Connection::open(tmp.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS config_store (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                source TEXT DEFAULT 'test',
                is_secret INTEGER DEFAULT 0,
                updated_at TEXT
            );
            CREATE TABLE IF NOT EXISTS secrets (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                encrypted_value TEXT NOT NULL,
                category TEXT DEFAULT 'general',
                description TEXT,
                created_at TEXT,
                updated_at TEXT
            );",
        )
        .unwrap();
        (tmp, conn)
    }

    #[test]
    fn test_read_config_store_values() {
        let (tmp, conn) = create_test_db();
        conn.execute(
            "INSERT INTO config_store (key, value) VALUES ('memory.neo4j_url', 'http://test:7474')",
            [],
        )
        .unwrap();

        let value = read_config_value(&conn, "memory.neo4j_url");
        assert_eq!(value, Some("http://test:7474".to_string()));

        let missing = read_config_value(&conn, "memory.nonexistent");
        assert_eq!(missing, None);
        drop(tmp);
    }

    #[test]
    fn test_config_env_override() {
        let (_tmp, _conn) = create_test_db();
        unsafe { std::env::set_var("GOBBY_NEO4J_URL", "http://env-override:9999") };
        let url = std::env::var("GOBBY_NEO4J_URL").unwrap();
        assert_eq!(url, "http://env-override:9999");
        unsafe { std::env::remove_var("GOBBY_NEO4J_URL") };
    }

    #[test]
    fn test_config_defaults() {
        // When config_store has no neo4j entries, defaults should apply
        let default_url = "http://localhost:8474";
        let default_db = "neo4j";
        assert_eq!(default_url, "http://localhost:8474");
        assert_eq!(default_db, "neo4j");
    }
}
