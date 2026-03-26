/// task_manager — Async task queue + preemption + timeout backfill
///
/// Manages tool call tasks initiated by the session. Supports concurrent execution, preemptive abort, and timeout notification.
/// Task results are sent back to the session via channel.
use crate::agent::tools;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::AppHandle;
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

// ============================================================
// Type definitions
// ============================================================

/// Single task request
#[derive(Debug, Clone)]
pub struct TaskRequest {
    /// Tool call ID (corresponds to the Realtime API's tool_call.id)
    pub call_id: String,
    pub tool_name: String,
    pub arguments: String,
}

/// Task completion result
#[derive(Debug, Clone)]
pub struct TaskResult {
    pub call_id: String,
    pub tool_name: String,
    pub output: String,
}

/// Events sent to the session
#[derive(Debug)]
pub enum TaskEvent {
    /// Task completed
    Completed(TaskResult),
    /// Intermediate progress (streaming report)
    Progress { call_id: String, message: String },
}

struct RunningTask {
    abort_handle: AbortHandle,
    child: Option<Arc<Mutex<Option<Child>>>>,
}

struct TaskMetrics {
    started: AtomicU64,
    completed: AtomicU64,
    aborted: AtomicU64,
    timed_out: AtomicU64,
}

impl TaskMetrics {
    fn new() -> Self {
        Self {
            started: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            aborted: AtomicU64::new(0),
            timed_out: AtomicU64::new(0),
        }
    }
}

struct ExecShellResult {
    output: String,
    timed_out: bool,
}

// ============================================================
// TaskManager
// ============================================================

pub struct TaskManager {
    app: AppHandle,
    event_tx: mpsc::Sender<TaskEvent>,
    /// Running tasks map: call_id → abort handle
    running: Arc<Mutex<HashMap<String, RunningTask>>>,
    metrics: Arc<TaskMetrics>,
}

impl TaskManager {
    pub fn new(app: AppHandle, event_tx: mpsc::Sender<TaskEvent>) -> Self {
        Self {
            app,
            event_tx,
            running: Arc::new(Mutex::new(HashMap::new())),
            metrics: Arc::new(TaskMetrics::new()),
        }
    }

    /// Submit one or more tasks for execution
    pub fn submit(&self, tasks: Vec<TaskRequest>) {
        for task in tasks {
            self.spawn_task(task);
        }
    }

    /// Abort a specific task
    #[allow(dead_code)]
    pub fn abort(&self, call_id: &str) {
        let mut running = self.running.lock() /* removed unwrap_or_else */ .unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
        if let Some(task) = running.remove(call_id) {
            if let Some(child) = task.child {
                kill_child_process(&child, call_id);
            }
            task.abort_handle.abort();
            self.metrics.aborted.fetch_add(1, Ordering::Relaxed);
            log::info!("[TaskManager] Aborted task: {}", call_id);
            self.log_metrics_snapshot();
        }
    }

    /// Abort all tasks
    pub fn abort_all(&self) {
        let mut running = self.running.lock() /* removed unwrap_or_else */ .unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
        for (id, task) in running.drain() {
            if let Some(child) = task.child {
                kill_child_process(&child, &id);
            }
            task.abort_handle.abort();
            self.metrics.aborted.fetch_add(1, Ordering::Relaxed);
            log::info!("[TaskManager] Aborted task: {}", id);
        }
        self.log_metrics_snapshot();
    }

    fn spawn_task(&self, task: TaskRequest) {
        let call_id = task.call_id.clone();
        let tool_name = task.tool_name.clone();
        let arguments = task.arguments.clone();
        let event_tx = self.event_tx.clone();
        let app = self.app.clone();
        
        let shell_child = if tool_name == "exec_shell" {
            Some(Arc::new(Mutex::new(None)))
        } else {
            None
        };
        let shell_child_for_task = shell_child.clone();

        let inner_call_id = call_id.clone();
        let inner_tool_name = tool_name.clone();
        let inner_metrics = self.metrics.clone();
        
        let worker_join = tokio::spawn(async move {
            let output = if tools::is_ui_tool(&inner_tool_name) {
                tools::dispatch_ui_tool(&app, &inner_tool_name, &arguments).await
            } else if inner_tool_name == "exec_shell" {
                let cid = inner_call_id.clone();
                let tx = event_tx.clone();
                let args_clone = arguments.clone();
                let child = shell_child_for_task.unwrap_or_else(|| Arc::new(Mutex::new(None)));
                let result = exec_shell_streaming(&args_clone, &cid, &tx, &child).await;
                if result.timed_out {
                     inner_metrics.timed_out.fetch_add(1, Ordering::Relaxed);
                }
                result.output
            } else {
                let name_clone = inner_tool_name.clone();
                let args_clone = arguments.clone();
                let app_clone = app.clone();
                match tokio::task::spawn_blocking(move || {
                    tools::dispatch_tool_with_app(&app_clone, &name_clone, &args_clone)
                })
                .await {
                    Ok(res) => res,
                    Err(e) => {
                        log::error!("[TaskManager] Blocking tool execution panicked: {}", e);
                        format!("System Error: Tool execution panicked.")
                    }
                }
            };
            output
        });

        let call_id_key = call_id.clone();
        let abort_handle = worker_join.abort_handle();
        
        {
            let mut r = self.running.lock().unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
            r.insert(
                call_id_key,
                RunningTask {
                    abort_handle,
                    child: shell_child,
                },
            );
        }
        
        self.metrics.started.fetch_add(1, Ordering::Relaxed);
        self.log_metrics_snapshot();

        let final_tx = self.event_tx.clone();
        let final_running = self.running.clone();
        let final_call_id = call_id.clone();
        let final_tool = tool_name.clone();
        let final_metrics = self.metrics.clone();
        
        tokio::spawn(async move {
            let final_output = match worker_join.await {
                Ok(output) => output,
                Err(e) => {
                    if e.is_cancelled() {
                        return; // Aborted by task manager
                    } else if e.is_panic() {
                        log::error!("[TaskManager] Task {} panicked!", final_call_id);
                        format!("System Error: Internal tool panic")
                    } else {
                        format!("System Error: Task failed")
                    }
                }
            };
            
            // Remove from running map
            {
                let mut r = final_running.lock().unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
                r.remove(&final_call_id);
            }
            
            let result = TaskResult {
                call_id: final_call_id,
                tool_name: final_tool,
                output: final_output,
            };
            let _ = final_tx.send(TaskEvent::Completed(result)).await;
            
            final_metrics.completed.fetch_add(1, Ordering::Relaxed);
            log::info!(
                "[TaskManager] Metrics started={} completed={} aborted={} timed_out={}",
                final_metrics.started.load(Ordering::Relaxed),
                final_metrics.completed.load(Ordering::Relaxed),
                final_metrics.aborted.load(Ordering::Relaxed),
                final_metrics.timed_out.load(Ordering::Relaxed)
            );
        });
    }

    fn log_metrics_snapshot(&self) {
        let started = self.metrics.started.load(Ordering::Relaxed);
        let completed = self.metrics.completed.load(Ordering::Relaxed);
        let aborted = self.metrics.aborted.load(Ordering::Relaxed);
        let timed_out = self.metrics.timed_out.load(Ordering::Relaxed);
        log::info!(
            "[TaskManager] Metrics started={} completed={} aborted={} timed_out={}",
            started,
            completed,
            aborted,
            timed_out
        );
    }
}

fn kill_child_process(child_ref: &Arc<Mutex<Option<Child>>>, call_id: &str) {
    let mut guard = child_ref.lock() /* removed unwrap_or_else */ .unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
    if let Some(child) = guard.as_mut() {
        if let Err(e) = child.start_kill() {
            log::warn!(
                "[TaskManager] Failed to kill process for task {}: {}",
                call_id,
                e
            );
        }
    }
}

// ============================================================
// Streaming shell execution
// ============================================================

/// Stream-execute a shell command, reporting every PROGRESS_LINES lines via Progress events
async fn exec_shell_streaming(
    args_json: &str,
    call_id: &str,
    event_tx: &mpsc::Sender<TaskEvent>,
    child_ref: &Arc<Mutex<Option<Child>>>,
) -> ExecShellResult {
    exec_shell_streaming_with_timeout(
        args_json,
        call_id,
        event_tx,
        child_ref,
        Duration::from_secs(crate::system_api::shell_timeout_secs()),
    )
    .await
}

async fn exec_shell_streaming_with_timeout(
    args_json: &str,
    call_id: &str,
    event_tx: &mpsc::Sender<TaskEvent>,
    child_ref: &Arc<Mutex<Option<Child>>>,
    timeout: Duration,
) -> ExecShellResult {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    const PROGRESS_LINES: usize = 10;

    let command = serde_json::from_str::<serde_json::Value>(args_json)
        .ok()
        .and_then(|v| v["command"].as_str().map(String::from))
        .unwrap_or_default();

    if command.is_empty() {
        return ExecShellResult {
            output: "Error: empty command".into(),
            timed_out: false,
        };
    }

    if let Err(e) = crate::system_api::validate_shell_command(&command, false) {
        return ExecShellResult {
            output: format!("Error: {}", e),
            timed_out: false,
        };
    }

    #[cfg(target_os = "windows")]
    let mut child = match Command::new("powershell")
        .args(["-NoProfile", "-Command", &command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return ExecShellResult {
                output: format!("Error: {}", e),
                timed_out: false,
            }
        }
    };

    #[cfg(not(target_os = "windows"))]
    let mut child = match Command::new("sh")
        .args(["-c", &command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return ExecShellResult {
                output: format!("Error: {}", e),
                timed_out: false,
            }
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    {
        let mut slot = child_ref.lock() /* removed unwrap_or_else */ .unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
        *slot = Some(child);
    }

    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<(bool, String)>(1024);

    if let Some(out) = stdout {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if tx.send((false, line)).await.is_err() {
                    break;
                }
            }
        });
    }

    if let Some(err) = stderr {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(err).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if tx.send((true, line)).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(line_tx);

    let mut all_output = String::new();
    let mut line_buf = Vec::new();

    let cid = call_id.to_string();
    let tx = event_tx.clone();

    let run_loop = async {
        while let Some((is_stderr, line)) = line_rx.recv().await {
            if is_stderr {
                all_output.push_str("[stderr] ");
                all_output.push_str(&line);
                all_output.push('\n');
                line_buf.push(format!("[stderr] {}", line));
            } else {
                all_output.push_str(&line);
                all_output.push('\n');
                line_buf.push(line);
            }

            if line_buf.len() >= PROGRESS_LINES {
                let chunk = line_buf.join("\n");
                let _ = tx.try_send(TaskEvent::Progress {
                    call_id: cid.clone(),
                    message: chunk,
                });
                line_buf.clear();
            }
        }

        if !line_buf.is_empty() {
            let chunk = line_buf.join("\n");
            let _ = tx.try_send(TaskEvent::Progress {
                call_id: cid.clone(),
                message: chunk,
            });
        }

        loop {
            {
                let mut guard = child_ref.lock() /* removed unwrap_or_else */ .unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
                if let Some(child) = guard.as_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => return Ok(status),
                        Ok(None) => {}
                        Err(e) => return Err(format!("Error waiting for process: {}", e)),
                    }
                } else {
                    return Err("Process handle unavailable".to_string());
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    };

    let exit_status = tokio::time::timeout(timeout, run_loop).await;

    {
        let mut guard = child_ref.lock() /* removed unwrap_or_else */ .unwrap_or_else(|e| { log::error!("[Mutex] Poisoned, state corrupted. Propagating panic."); panic!("Mutex poisoned: {}", e); });
        *guard = None;
    }

    let mut timed_out = false;
    match exit_status {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            all_output.push_str(&format!("\nError: {}", e));
        }
        Err(_) => {
            timed_out = true;
            all_output.push_str(&format!(
                "\nError: Command timed out after {} seconds",
                timeout.as_secs()
            ));
            kill_child_process(child_ref, call_id);
        }
    }

    let limit = 8000;
    if all_output.len() > limit {
        let half = limit / 2;
        let start_part = &all_output[..half];

        let mut end_start = all_output.len() - half;
        while end_start < all_output.len() && !all_output.is_char_boundary(end_start) {
            end_start += 1;
        }
        let end_part = &all_output[end_start..];

        let omitted = all_output.len() - start_part.len() - end_part.len();
        all_output = format!("{}

...[Output Truncated: {} bytes omitted. Use specific search (grep/Select-String) if you need details]...

{}", start_part, omitted, end_part);
    }

    ExecShellResult {
        output: all_output,
        timed_out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_event_tx() -> mpsc::Sender<TaskEvent> {
        let (tx, _rx) = mpsc::channel::<TaskEvent>(8);
        tx
    }

    #[tokio::test]
    async fn exec_shell_streaming_returns_output_for_short_command() {
        let child_ref = Arc::new(Mutex::new(None));
        let tx = dummy_event_tx();
        let args_json = serde_json::json!({
            "command": if cfg!(target_os = "windows") { "Write-Output 'ok'" } else { "echo ok" }
        })
        .to_string();

        let result = exec_shell_streaming_with_timeout(
            &args_json,
            "test-call",
            &tx,
            &child_ref,
            std::time::Duration::from_secs(5),
        )
        .await;

        assert!(!result.timed_out);
        assert!(result.output.to_lowercase().contains("ok"));
    }

    #[tokio::test]
    async fn exec_shell_streaming_times_out_for_long_command() {
        let child_ref = Arc::new(Mutex::new(None));
        let tx = dummy_event_tx();
        let args_json = serde_json::json!({
            "command": if cfg!(target_os = "windows") { "Start-Sleep -Seconds 5" } else { "sleep 5" }
        })
        .to_string();

        let result = exec_shell_streaming_with_timeout(
            &args_json,
            "test-timeout",
            &tx,
            &child_ref,
            std::time::Duration::from_secs(1),
        )
        .await;

        assert!(result.timed_out);
        assert!(result.output.contains("Command timed out"));
    }
}
