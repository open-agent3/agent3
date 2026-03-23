/// scheduler — Scheduled task framework: supports Agent self-setting reminders and timed tasks
///
/// Task definitions are stored in DB (scheduled_tasks table); triggers are sent to the session via inject channel when due.
/// Agent can set new tasks through the schedule_task tool.
use crate::db::DbState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc;

// ============================================================
// Type definitions
// ============================================================

/// Scheduled task event → bridged to session via inject channel
#[derive(Debug)]
pub enum SchedulerEvent {
    /// Scheduled task triggered
    TaskDue {
        #[allow(dead_code)]
        id: i64,
        description: String,
    },
}

/// Scheduler handle
pub struct SchedulerHandle {
    task: tokio::task::JoinHandle<()>,
    stop_flag: Arc<AtomicBool>,
}

impl SchedulerHandle {
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        self.task.abort();
    }
}

// ============================================================
// Start the scheduled task dispatcher
// ============================================================

pub fn start(app: AppHandle, event_tx: mpsc::Sender<SchedulerEvent>) -> SchedulerHandle {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let flag_clone = stop_flag.clone();

    let task = tokio::spawn(async move {
        scheduler_loop(app, event_tx, flag_clone).await;
    });

    SchedulerHandle { task, stop_flag }
}

// ============================================================
// Core loop — scans for pending tasks every 30 seconds
// ============================================================

async fn scheduler_loop(
    app: AppHandle,
    event_tx: mpsc::Sender<SchedulerEvent>,
    stop_flag: Arc<AtomicBool>,
) {
    log::info!("[Scheduler] Started");

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    interval.tick().await; // Skip the immediate first tick

    loop {
        interval.tick().await;

        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        // Query tasks that are due and not yet completed
        let due_tasks = {
            let db_state = app.state::<DbState>();
            let pool = &db_state.0;
            let rows = sqlx::query_as::<_, (i64, String)>(
                "SELECT id, description FROM scheduled_tasks \
                 WHERE due_at <= datetime('now') AND completed = 0 \
                 ORDER BY due_at LIMIT 5",
            )
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            rows
        };

        for (id, description) in due_tasks {
            log::info!("[Scheduler] Task #{} due: {}", id, description);

            // Mark as completed
            {
                let db_state = app.state::<DbState>();
                let pool = &db_state.0;
                let _ = sqlx::query("UPDATE scheduled_tasks SET completed = 1 WHERE id = ?")
                    .bind(id)
                    .execute(pool)
                    .await;
            }

            // Send to session via inject channel
            let _ = event_tx
                .send(SchedulerEvent::TaskDue { id, description })
                .await;
        }
    }

    log::info!("[Scheduler] Stopped");
}

// ============================================================
// Tool interface — called by tools.rs
// ============================================================

/// Add a scheduled task to the DB
/// due_in_seconds: seconds from now until the task triggers
/// Returns the task ID
pub async fn add_scheduled_task(
    app: &AppHandle,
    description: &str,
    due_in_seconds: i64,
) -> Result<i64, String> {
    let db_state = app.state::<DbState>();
    let pool = &db_state.0;

    let result = sqlx::query(
        "INSERT INTO scheduled_tasks (description, due_at) \
         VALUES (?1, datetime('now', '+' || ?2 || ' seconds'))",
    )
    .bind(description)
    .bind(due_in_seconds)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(result.last_insert_rowid())
}

/// List all pending tasks
#[allow(dead_code)]
pub async fn list_pending_tasks(app: &AppHandle) -> Vec<(i64, String, String)> {
    let db_state = app.state::<DbState>();
    let pool = &db_state.0;

    sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, description, due_at FROM scheduled_tasks \
         WHERE completed = 0 ORDER BY due_at",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}
