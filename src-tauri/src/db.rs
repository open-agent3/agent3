/// db — SQLite persistence module using sqlx
///
/// Uses PRAGMA user_version for incremental migrations.
/// Automatically detects and runs pending migrations on each app startup.
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqlitePool, SqlitePoolOptions},
    Row,
};
use std::path::PathBuf;

// ============================================================
// Global DB state (Tauri State managed)
// ============================================================

pub struct DbState(pub SqlitePool);

/// Get the database file path: <app_data_dir>/agent3.db
pub fn db_path(app: &tauri::AppHandle) -> PathBuf {
    let dir = app
        .path()
        .app_data_dir()
        .expect("failed to resolve app data dir");
    std::fs::create_dir_all(&dir).expect("failed to create app data dir");
    dir.join("agent3.db")
}

use tauri::Manager;

/// Initialize database connection pool and run migrations
pub async fn init_db(app: &tauri::AppHandle) -> Result<SqlitePool, sqlx::Error> {
    let path = db_path(app);

    // One-time migration: move legacy DB filename to the new default name.
    // This keeps existing local data when the filename convention changes.
    if !path.exists() {
        let legacy_path = path.with_file_name("agent3_v2.db");
        if legacy_path.exists() {
            match std::fs::rename(&legacy_path, &path) {
                Ok(()) => log::info!(
                    "[DB] Migrated legacy DB filename: {} -> {}",
                    legacy_path.display(),
                    path.display()
                ),
                Err(e) => {
                    log::warn!(
                        "[DB] Failed to rename legacy DB ({}): {}. Falling back to copy.",
                        legacy_path.display(),
                        e
                    );
                    if let Err(copy_err) = std::fs::copy(&legacy_path, &path) {
                        log::warn!(
                            "[DB] Failed to copy legacy DB ({} -> {}): {}",
                            legacy_path.display(),
                            path.display(),
                            copy_err
                        );
                    }
                }
            }
        }
    }

    // sqlx requires the db to exist before connecting unless SQLite-specific connect options are used
    if !path.exists() {
        std::fs::File::create(&path).expect("failed to create sqlite file");
    }

    let url = format!("sqlite://{}", path.display());

    // Create the pool with WAL mode enabled
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("PRAGMA journal_mode=WAL;")
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await?;

    run_migrations(&pool).await?;
    Ok(pool)
}

// ============================================================
// Migration system
// ============================================================

/// Migration function list. Index = version number - 1.
/// To add a new migration, simply append a function to the end of the array.
const MIGRATIONS: &[&str] = &[MIGRATE_V1, MIGRATE_V2];

const MIGRATE_V1: &str = "
CREATE TABLE IF NOT EXISTS llm_providers (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    base_url      TEXT NOT NULL,
    api_key       TEXT NOT NULL DEFAULT '',
    model         TEXT NOT NULL DEFAULT '',
    is_active     INTEGER NOT NULL DEFAULT 0,
    provider_type TEXT NOT NULL DEFAULT 'openai',
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS app_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS scheduled_tasks (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    description TEXT NOT NULL,
    due_at      TEXT NOT NULL,
    completed   INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_scheduled_due ON scheduled_tasks(due_at) WHERE completed = 0;

CREATE TABLE IF NOT EXISTS core_profile (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS episodic_logs (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_episodic_created ON episodic_logs(created_at);
CREATE INDEX IF NOT EXISTS idx_episodic_session ON episodic_logs(session_id);

CREATE TABLE IF NOT EXISTS kg_nodes (
    id TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    type TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS kg_edges (
    source_id TEXT NOT NULL,
    target_id TEXT NOT NULL,
    relation TEXT NOT NULL,
    PRIMARY KEY (source_id, target_id, relation),
    FOREIGN KEY (source_id) REFERENCES kg_nodes(id) ON DELETE CASCADE,
    FOREIGN KEY (target_id) REFERENCES kg_nodes(id) ON DELETE CASCADE
);

CREATE VIRTUAL TABLE IF NOT EXISTS kg_nodes_fts USING fts5(
    label,
    type,
    content='kg_nodes',
    content_rowid='rowid',
    tokenize = 'unicode61'
);

-- Triggers to keep kg_nodes_fts updated
CREATE TRIGGER IF NOT EXISTS kg_nodes_ai AFTER INSERT ON kg_nodes BEGIN
    INSERT INTO kg_nodes_fts(rowid, label, type) VALUES (new.rowid, new.label, new.type);
END;
CREATE TRIGGER IF NOT EXISTS kg_nodes_ad AFTER DELETE ON kg_nodes BEGIN
    INSERT INTO kg_nodes_fts(kg_nodes_fts, rowid, label, type) VALUES('delete', old.rowid, old.label, old.type);
END;
CREATE TRIGGER IF NOT EXISTS kg_nodes_au AFTER UPDATE ON kg_nodes BEGIN
    INSERT INTO kg_nodes_fts(kg_nodes_fts, rowid, label, type) VALUES('delete', old.rowid, old.label, old.type);
    INSERT INTO kg_nodes_fts(rowid, label, type) VALUES (new.rowid, new.label, new.type);
END;
";

const MIGRATE_V2: &str = "
-- Try to add the role column if it does not exist.
ALTER TABLE llm_providers ADD COLUMN role TEXT NOT NULL DEFAULT 'realtime';
CREATE INDEX IF NOT EXISTS idx_llm_providers_role_active ON llm_providers(role, is_active);
";

async fn run_migrations(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    // Get current version
    let current: i32 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(pool)
        .await
        .unwrap_or(0);

    let target = MIGRATIONS.len() as i32;

    for v in current..target {
        let migration_sql = MIGRATIONS[v as usize];
        if v == 1 {
            // v2
            let _ = sqlx::query(migration_sql).execute(pool).await;
        } else {
            sqlx::query(migration_sql).execute(pool).await?;
        }

        let next_version = v + 1;
        let pragma = format!("PRAGMA user_version = {}", next_version);
        sqlx::query(&pragma).execute(pool).await?;
    }

    Ok(())
}

// ============================================================
// Data structures
// ============================================================

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct LlmProvider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub is_active: bool,
    pub provider_type: String,
    #[serde(default = "default_provider_role")]
    pub role: String,
}

fn default_provider_role() -> String {
    "realtime".to_string()
}

// ============================================================
// Tauri Commands — LLM Providers
// ============================================================

#[tauri::command]
pub async fn get_providers(state: tauri::State<'_, DbState>) -> Result<Vec<LlmProvider>, String> {
    let pool = &state.0;
    let rows = sqlx::query(
        "SELECT id, name, base_url, api_key, model, is_active, provider_type, role FROM llm_providers ORDER BY created_at"
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    let providers = rows
        .iter()
        .map(|row| LlmProvider {
            id: row.get(0),
            name: row.get(1),
            base_url: row.get(2),
            api_key: row.get(3),
            model: row.get(4),
            is_active: row.get::<i64, _>(5) != 0,
            provider_type: row.get(6),
            role: row.get(7),
        })
        .collect();

    Ok(providers)
}

#[tauri::command]
pub async fn save_provider(
    state: tauri::State<'_, DbState>,
    provider: LlmProvider,
) -> Result<(), String> {
    let pool = &state.0;
    sqlx::query(
        "INSERT INTO llm_providers (id, name, base_url, api_key, model, is_active, provider_type, role)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
           name=excluded.name, base_url=excluded.base_url,
           api_key=excluded.api_key, model=excluded.model,
           is_active=excluded.is_active, provider_type=excluded.provider_type, role=excluded.role,
           updated_at=datetime('now')"
    )
    .bind(provider.id)
    .bind(provider.name)
    .bind(provider.base_url)
    .bind(provider.api_key)
    .bind(provider.model)
    .bind(provider.is_active as i64)
    .bind(provider.provider_type)
    .bind(provider.role)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn delete_provider(state: tauri::State<'_, DbState>, id: String) -> Result<(), String> {
    let pool = &state.0;
    sqlx::query("DELETE FROM llm_providers WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn set_active_provider(
    state: tauri::State<'_, DbState>,
    id: String,
) -> Result<(), String> {
    let pool = &state.0;
    sqlx::query("UPDATE llm_providers SET is_active = 0")
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    sqlx::query("UPDATE llm_providers SET is_active = 1 WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn get_active_provider(
    state: tauri::State<'_, DbState>,
) -> Result<Option<LlmProvider>, String> {
    let pool = &state.0;
    let row = sqlx::query(
        "SELECT id, name, base_url, api_key, model, is_active, provider_type, role FROM llm_providers WHERE is_active = 1 AND role IN ('realtime', 'sensory') LIMIT 1"
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;

    match row {
        Some(r) => Ok(Some(LlmProvider {
            id: r.get(0),
            name: r.get(1),
            base_url: r.get(2),
            api_key: r.get(3),
            model: r.get(4),
            is_active: true,
            provider_type: r.get(6),
            role: r.get(7),
        })),
        None => Ok(None),
    }
}

// ============================================================
// Tauri Commands — App Settings (KV Store)
// ============================================================

#[tauri::command]
pub async fn get_setting(
    state: tauri::State<'_, DbState>,
    key: String,
) -> Result<Option<String>, String> {
    let pool = &state.0;
    let row = sqlx::query_scalar::<_, String>("SELECT value FROM app_settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(row)
}

#[tauri::command]
pub async fn set_setting(
    state: tauri::State<'_, DbState>,
    key: String,
    value: String,
) -> Result<(), String> {
    let pool = &state.0;
    sqlx::query(
        "INSERT INTO app_settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value=excluded.value"
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

// ============================================================
// Cognitive Architecture DB Wrapper Structs
// ============================================================

#[derive(Serialize, Deserialize, Debug)]
pub struct CoreProfileEntry {
    pub key: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EpisodicLog {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct KgNode {
    pub id: String,
    pub label: String,
    pub r#type: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct KgEdge {
    pub source_id: String,
    pub target_id: String,
    pub relation: String,
}

// ============================================================
// Cognitive Architecture Getter/Setter Functions
// ============================================================

pub async fn upsert_core_profile(
    pool: &SqlitePool,
    key: &str,
    value: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO core_profile (key, value) VALUES (?, ?)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_core_profile(pool: &SqlitePool) -> Result<Vec<CoreProfileEntry>, sqlx::Error> {
    let rows = sqlx::query("SELECT key, value FROM core_profile")
        .fetch_all(pool)
        .await?;

    let results = rows
        .iter()
        .map(|row| CoreProfileEntry {
            key: row.get(0),
            value: row.get(1),
        })
        .collect();

    Ok(results)
}

pub async fn add_episodic_log(
    pool: &SqlitePool,
    id: &str,
    session_id: &str,
    role: &str,
    content: &str,
    created_at: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO episodic_logs (id, session_id, role, content, created_at) VALUES (?, ?, ?, ?, ?)"
    )
    .bind(id)
    .bind(session_id)
    .bind(role)
    .bind(content)
    .bind(created_at)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn get_recent_episodes(
    pool: &SqlitePool,
    limit: usize,
) -> Result<Vec<EpisodicLog>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, session_id, role, content, created_at FROM episodic_logs ORDER BY created_at DESC LIMIT ?"
    )
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;

    let mut results: Vec<EpisodicLog> = rows
        .iter()
        .map(|row| EpisodicLog {
            id: row.get(0),
            session_id: row.get(1),
            role: row.get(2),
            content: row.get(3),
            created_at: row.get(4),
        })
        .collect();

    results.reverse();
    Ok(results)
}

pub async fn add_kg_node(
    pool: &SqlitePool,
    id: &str,
    label: &str,
    node_type: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO kg_nodes (id, label, type) VALUES (?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET label=excluded.label, type=excluded.type",
    )
    .bind(id)
    .bind(label)
    .bind(node_type)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn add_kg_edge(
    pool: &SqlitePool,
    source_id: &str,
    target_id: &str,
    relation: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO kg_edges (source_id, target_id, relation) VALUES (?, ?, ?)
         ON CONFLICT(source_id, target_id, relation) DO NOTHING",
    )
    .bind(source_id)
    .bind(target_id)
    .bind(relation)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn search_kg_subgraph(
    pool: &SqlitePool,
    keyword: &str,
    limit: usize,
) -> Result<(Vec<KgNode>, Vec<KgEdge>), sqlx::Error> {
    let fts_query = format!("{}*", keyword);

    let rows = sqlx::query(
        "SELECT id, label, type FROM kg_nodes
         WHERE rowid IN (
             SELECT rowid FROM kg_nodes_fts WHERE kg_nodes_fts MATCH ? LIMIT ?
         )",
    )
    .bind(&fts_query)
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;

    let nodes: Vec<KgNode> = rows
        .iter()
        .map(|row| KgNode {
            id: row.get(0),
            label: row.get(1),
            r#type: row.get(2),
        })
        .collect();

    if nodes.is_empty() {
        return Ok((nodes, Vec::new()));
    }

    let mut edges = Vec::new();
    for node in &nodes {
        let edge_rows = sqlx::query(
            "SELECT source_id, target_id, relation FROM kg_edges WHERE source_id = ? OR target_id = ?"
        )
        .bind(&node.id)
        .bind(&node.id)
        .fetch_all(pool)
        .await?;

        for row in edge_rows {
            edges.push(KgEdge {
                source_id: row.get(0),
                target_id: row.get(1),
                relation: row.get(2),
            });
        }
    }

    edges.sort_by(|a, b| {
        a.source_id
            .cmp(&b.source_id)
            .then(a.target_id.cmp(&b.target_id))
            .then(a.relation.cmp(&b.relation))
    });
    edges.dedup_by(|a, b| {
        a.source_id == b.source_id && a.target_id == b.target_id && a.relation == b.relation
    });

    Ok((nodes, edges))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn setup_test_db() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("Failed to connect to in-memory db");

        run_migrations(&pool)
            .await
            .expect("Failed to run migrations");
        pool
    }

    #[tokio::test]
    async fn test_setup_and_migrations() {
        let pool = setup_test_db().await;

        let current: i32 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .unwrap_or(0);

        assert_eq!(current, MIGRATIONS.len() as i32);
    }

    #[tokio::test]
    async fn test_crud_operations() {
        let pool = setup_test_db().await;

        // Test Core Profile
        upsert_core_profile(&pool, "test_key", "test_value")
            .await
            .unwrap();
        let profiles = get_core_profile(&pool).await.unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].key, "test_key");
        assert_eq!(profiles[0].value, "test_value");

        // Test Episodic Log
        add_episodic_log(&pool, "log1", "sess1", "user", "test_content", 1000)
            .await
            .unwrap();
        let logs = get_recent_episodes(&pool, 10).await.unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].id, "log1");
        assert_eq!(logs[0].content, "test_content");
        assert_eq!(logs[0].created_at, 1000);

        // Test KG Nodes / Edges
        add_kg_node(&pool, "n1", "Node 1", "Concept").await.unwrap();
        add_kg_node(&pool, "n2", "Node 2", "Concept").await.unwrap();
        add_kg_edge(&pool, "n1", "n2", "relates_to").await.unwrap();

        let (nodes, edges) = search_kg_subgraph(&pool, "Node", 10).await.unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(edges.len(), 1);
    }
}
