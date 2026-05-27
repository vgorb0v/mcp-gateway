use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::json;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use uuid::Uuid;

use super::transport::{BackendHealth, BackendNotification, BackendTransport};
use crate::config::{DefaultsConfig, LimitsConfig, RestartPolicy, ServerConfig};
use crate::error::{GatewayError, Result};
use crate::jsonrpc::{
    JsonRpcId, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
    METHOD_INITIALIZE, METHOD_INITIALIZED, METHOD_TOOLS_CALL,
};
use crate::logs::LogStore;
use crate::metrics::{BackendRpcMetrics, ResourceSample, RpcMetricsSnapshot};

#[derive(Clone)]
pub struct StdioBackend {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    config: ServerConfig,
    defaults: DefaultsConfig,
    limits: LimitsConfig,
    logs: LogStore,
    state: Mutex<State>,
    start_lock: Mutex<()>,
    init_lock: Mutex<()>,
    pending: Arc<Mutex<BTreeMap<JsonRpcId, PendingRequest>>>,
    notifications: broadcast::Sender<BackendNotification>,
    metrics: BackendRpcMetrics,
    /// Latest resource sample populated by the metrics sampler task. Reading
    /// it during `health()` is O(1) and never spawns a subprocess — a 5-second
    /// lag is acceptable for status display. Uses `parking_lot::Mutex` so
    /// the read path is wait-free under contention.
    last_sample: parking_lot::Mutex<Option<ResourceSample>>,
}

struct PendingRequest {
    sender: oneshot::Sender<JsonRpcResponse>,
    started_at: Instant,
    client_id: JsonRpcId,
}

struct State {
    child: Option<Child>,
    writer: Option<mpsc::Sender<String>>,
    pid: Option<u32>,
    started_at: Option<Instant>,
    last_used: Instant,
    active_requests: usize,
    initialized: bool,
    state: LifecycleState,
    last_error: Option<String>,
    restart_attempts: VecDeque<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleState {
    Stopped,
    Starting,
    Running,
    Unhealthy,
}

impl LifecycleState {
    fn as_str(self) -> &'static str {
        match self {
            LifecycleState::Stopped => "stopped",
            LifecycleState::Starting => "starting",
            LifecycleState::Running => "running",
            LifecycleState::Unhealthy => "unhealthy",
        }
    }
}

impl StdioBackend {
    pub fn new(
        name: impl Into<String>,
        config: ServerConfig,
        defaults: DefaultsConfig,
        limits: LimitsConfig,
        logs: LogStore,
    ) -> Self {
        let (notifications, _) = broadcast::channel(limits.backend_notification_broadcast);
        Self {
            inner: Arc::new(Inner {
                name: name.into(),
                config,
                defaults,
                limits,
                logs,
                state: Mutex::new(State {
                    child: None,
                    writer: None,
                    pid: None,
                    started_at: None,
                    last_used: Instant::now(),
                    active_requests: 0,
                    initialized: false,
                    state: LifecycleState::Stopped,
                    last_error: None,
                    restart_attempts: VecDeque::new(),
                }),
                start_lock: Mutex::new(()),
                init_lock: Mutex::new(()),
                pending: Arc::new(Mutex::new(BTreeMap::new())),
                notifications,
                metrics: BackendRpcMetrics::new(),
                last_sample: parking_lot::Mutex::new(None),
            }),
        }
    }

    pub fn metrics_snapshot(&self) -> RpcMetricsSnapshot {
        self.inner.metrics.snapshot()
    }

    async fn refresh_state(&self) {
        let mut state = self.inner.state.lock().await;
        if let Some(child) = state.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let crashed = !status.success();
                    state.child = None;
                    state.writer = None;
                    state.pid = None;
                    state.started_at = None;
                    state.initialized = false;
                    state.state = if crashed {
                        LifecycleState::Unhealthy
                    } else {
                        LifecycleState::Stopped
                    };
                    if crashed {
                        state.last_error = Some(format!("process exited with {status}"));
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    state.last_error = Some(err.to_string());
                    state.state = LifecycleState::Unhealthy;
                }
            }
        }
    }

    async fn ensure_initialized(&self) -> Result<()> {
        self.start().await?;
        {
            let state = self.inner.state.lock().await;
            if state.initialized {
                return Ok(());
            }
        }

        let _guard = self.inner.init_lock.lock().await;
        {
            let state = self.inner.state.lock().await;
            if state.initialized {
                return Ok(());
            }
        }

        let id = JsonRpcId::String(format!("gateway-init-{}", Uuid::new_v4()));
        let init = JsonRpcRequest::new(
            id,
            METHOD_INITIALIZE,
            Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "mcp-gateway",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
        );
        let response = self
            .send_raw_request(
                init,
                Duration::from_secs(
                    self.inner
                        .config
                        .startup_timeout_seconds(&self.inner.defaults),
                ),
            )
            .await?;
        if let Some(error) = response.error {
            return Err(GatewayError::Backend(format!(
                "backend initialize failed: {}",
                error.message
            )));
        }
        self.send_raw_notification(JsonRpcNotification::new(METHOD_INITIALIZED, None))
            .await?;
        let mut state = self.inner.state.lock().await;
        state.initialized = true;
        Ok(())
    }

    async fn send_raw_request(
        &self,
        mut request: JsonRpcRequest,
        timeout_duration: Duration,
    ) -> Result<JsonRpcResponse> {
        self.start().await?;
        let writer = {
            let mut state = self.inner.state.lock().await;
            state.active_requests += 1;
            state.last_used = Instant::now();
            state.writer.clone().ok_or_else(|| {
                GatewayError::Backend(format!("backend '{}' is not running", self.inner.name))
            })?
        };

        let client_id = request.id.clone();
        let backend_id = JsonRpcId::String(format!("gateway-request-{}", Uuid::new_v4()));
        request.id = backend_id.clone();
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.inner.pending.lock().await;
            if pending.len() >= self.inner.limits.max_pending_requests_per_backend {
                drop(pending);
                self.finish_request().await;
                return Err(GatewayError::Backend(format!(
                    "backend '{}' pending request limit {} reached",
                    self.inner.name, self.inner.limits.max_pending_requests_per_backend
                )));
            }
            pending.insert(
                backend_id.clone(),
                PendingRequest {
                    sender: tx,
                    started_at: Instant::now(),
                    client_id,
                },
            );
        }
        let encoded = format!("{}\n", serde_json::to_string(&request)?);
        let tool_name = if request.method == METHOD_TOOLS_CALL {
            request
                .params
                .as_ref()
                .and_then(|params| params.get("name"))
                .and_then(|name| name.as_str())
                .map(str::to_string)
        } else {
            None
        };
        self.inner.metrics.record_outbound_request(
            &request.method,
            tool_name.as_deref(),
            encoded.len(),
        );
        let send_result = writer.send(encoded).await.map_err(|_| {
            GatewayError::Backend(format!("backend '{}' stdin is closed", self.inner.name))
        });

        if let Err(err) = send_result {
            self.inner.pending.lock().await.remove(&backend_id);
            self.inner.metrics.record_request_dropped();
            self.finish_request().await;
            return Err(err);
        }

        let result = tokio::time::timeout(timeout_duration, rx)
            .await
            .map_err(|_| {
                GatewayError::Timeout(format!(
                    "backend '{}' timed out waiting for response",
                    self.inner.name
                ))
            })
            .and_then(|rx| {
                rx.map_err(|_| {
                    GatewayError::Backend(format!(
                        "backend '{}' response channel closed",
                        self.inner.name
                    ))
                })
            });
        if result.is_err() {
            // Pending entry was already removed via `record_response` when the
            // reader matched the response; if we got here on timeout/closed
            // channel, remove the entry now and account for the dropped pending.
            if self
                .inner
                .pending
                .lock()
                .await
                .remove(&backend_id)
                .is_some()
            {
                self.inner.metrics.record_request_dropped();
            }
        }
        self.finish_request().await;
        result
    }

    fn record_restart_attempt(&self, state: &mut State) -> Result<()> {
        if state.state != LifecycleState::Unhealthy {
            return Ok(());
        }
        if matches!(
            self.inner.config.restart_policy(&self.inner.defaults),
            RestartPolicy::Never
        ) {
            return Err(GatewayError::Backend(format!(
                "backend '{}' is unhealthy and restart policy is never",
                self.inner.name
            )));
        }

        let now = Instant::now();
        let window = Duration::from_secs(self.inner.limits.restart_window_seconds);
        while state
            .restart_attempts
            .front()
            .map(|attempt| now.duration_since(*attempt) > window)
            .unwrap_or(false)
        {
            state.restart_attempts.pop_front();
        }
        if state.restart_attempts.len() >= self.inner.limits.max_restart_attempts {
            let message = format!(
                "backend '{}' restart limit {} reached within {}s",
                self.inner.name,
                self.inner.limits.max_restart_attempts,
                self.inner.limits.restart_window_seconds
            );
            state.last_error = Some(message.clone());
            return Err(GatewayError::Backend(message));
        }
        state.restart_attempts.push_back(now);
        Ok(())
    }

    async fn finish_request(&self) {
        let mut state = self.inner.state.lock().await;
        state.active_requests = state.active_requests.saturating_sub(1);
        state.last_used = Instant::now();
    }

    async fn send_raw_notification(&self, notification: JsonRpcNotification) -> Result<()> {
        self.start().await?;
        let writer = {
            let mut state = self.inner.state.lock().await;
            state.last_used = Instant::now();
            state.writer.clone().ok_or_else(|| {
                GatewayError::Backend(format!("backend '{}' is not running", self.inner.name))
            })?
        };
        let encoded = format!("{}\n", serde_json::to_string(&notification)?);
        self.inner
            .metrics
            .record_outbound_notification(&notification.method, encoded.len());
        writer.send(encoded).await.map_err(|_| {
            GatewayError::Backend(format!("backend '{}' stdin is closed", self.inner.name))
        })
    }

    fn spawn_idle_watcher(&self) {
        let backend = self.clone();
        tokio::spawn(async move {
            let idle = Duration::from_secs(
                backend
                    .inner
                    .config
                    .idle_timeout_seconds(&backend.inner.defaults),
            );
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let should_stop = {
                    let state = backend.inner.state.lock().await;
                    state.child.is_some()
                        && state.active_requests == 0
                        && state.last_used.elapsed() >= idle
                        && backend.inner.config.lazy()
                };
                if should_stop {
                    let _ = backend.stop().await;
                    break;
                }
                let running = {
                    let state = backend.inner.state.lock().await;
                    state.child.is_some()
                };
                if !running {
                    break;
                }
            }
        });
    }
}

#[async_trait]
impl BackendTransport for StdioBackend {
    fn name(&self) -> &str {
        &self.inner.name
    }

    async fn start(&self) -> Result<()> {
        let _guard = self.inner.start_lock.lock().await;
        self.refresh_state().await;
        {
            let state = self.inner.state.lock().await;
            if state.child.is_some() {
                return Ok(());
            }
        }

        let mut state = self.inner.state.lock().await;
        if state.child.is_some() {
            return Ok(());
        }
        self.record_restart_attempt(&mut state)?;
        state.state = LifecycleState::Starting;
        drop(state);

        if let Some(url) = browser_debug_url(&self.inner.config) {
            if let Err(err) = ensure_browser_debug_url(&url).await {
                let mut state = self.inner.state.lock().await;
                state.state = LifecycleState::Stopped;
                state.last_error = Some(err.to_string());
                return Err(err);
            }
        }

        let mut command = Command::new(&self.inner.config.command);
        command
            .args(&self.inner.config.args)
            .envs(&self.inner.config.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        #[cfg(unix)]
        {
            command.process_group(0);
        }

        let mut child = command.spawn().map_err(|err| {
            GatewayError::Backend(format!(
                "failed to start backend '{}': {err}",
                self.inner.name
            ))
        })?;
        let pid = child.id();
        if let Some(pid) = pid {
            apply_backend_resource_policy(pid);
        }
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| GatewayError::Backend("child stdin missing".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| GatewayError::Backend("child stdout missing".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| GatewayError::Backend("child stderr missing".to_string()))?;

        let (writer, mut receiver) = mpsc::channel::<String>(self.inner.limits.backend_stdin_queue);
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = receiver.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        let pending = self.inner.pending.clone();
        let notifications = self.inner.notifications.clone();
        let logs = self.inner.logs.clone();
        let server_name = self.inner.name.clone();
        let max_stdout_line_bytes = self.inner.limits.max_message_bytes;
        let metrics = self.inner.metrics.clone();
        tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout);
            let mut line = Vec::with_capacity(1024);
            loop {
                match read_capped_line(&mut stdout, &mut line, max_stdout_line_bytes).await {
                    Ok(Some(true)) => {
                        logs.append(
                            &server_name,
                            "stdout",
                            format!("backend JSON-RPC line exceeded {max_stdout_line_bytes} bytes"),
                        );
                    }
                    Ok(Some(false)) => {
                        metrics.record_inbound_bytes(line.len());
                        match serde_json::from_slice::<JsonRpcMessage>(&line) {
                            Ok(JsonRpcMessage::Response(mut response)) => {
                                if let Some(entry) = pending.lock().await.remove(&response.id) {
                                    let latency = entry.started_at.elapsed();
                                    let error_message =
                                        response.error.as_ref().map(|err| err.message.clone());
                                    response.id = entry.client_id;
                                    metrics.record_response(
                                        latency,
                                        response.error.is_some(),
                                        error_message.as_deref(),
                                    );
                                    let _ = entry.sender.send(response);
                                }
                            }
                            Ok(JsonRpcMessage::Notification(notification)) => {
                                let _ = notifications.send(BackendNotification {
                                    server: server_name.clone(),
                                    notification,
                                });
                            }
                            Ok(JsonRpcMessage::Request(request)) => {
                                logs.append(
                                    &server_name,
                                    "stdout",
                                    format!("unsupported backend request '{}'", request.method),
                                );
                            }
                            Err(err) => logs.append(
                                &server_name,
                                "stdout",
                                format!(
                                    "invalid JSON-RPC from backend: {err}: {}",
                                    String::from_utf8_lossy(&line)
                                ),
                            ),
                        }
                    }
                    Ok(None) => break,
                    Err(err) => {
                        logs.append(&server_name, "stdout", format!("stdout read error: {err}"));
                        break;
                    }
                }
            }
            pending.lock().await.clear();
        });

        let logs = self.inner.logs.clone();
        let server_name = self.inner.name.clone();
        let max_stderr_line_bytes = self.inner.limits.max_log_line_bytes;
        tokio::spawn(async move {
            let mut stderr = BufReader::new(stderr);
            let mut line = Vec::with_capacity(1024);
            loop {
                match read_capped_line(&mut stderr, &mut line, max_stderr_line_bytes).await {
                    Ok(Some(_)) => {
                        logs.append(&server_name, "stderr", String::from_utf8_lossy(&line))
                    }
                    Ok(None) => break,
                    Err(err) => {
                        logs.append(&server_name, "stderr", format!("stderr read error: {err}"));
                        break;
                    }
                }
            }
        });

        let mut state = self.inner.state.lock().await;
        state.child = Some(child);
        state.writer = Some(writer);
        state.pid = pid;
        state.started_at = Some(Instant::now());
        state.state = LifecycleState::Running;
        state.last_error = None;
        state.last_used = Instant::now();
        drop(state);
        self.spawn_idle_watcher();
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let (mut child, pid) = {
            let mut state = self.inner.state.lock().await;
            let pid = state.pid;
            state.writer = None;
            state.initialized = false;
            state.state = LifecycleState::Stopped;
            state.pid = None;
            state.started_at = None;
            (state.child.take(), pid)
        };

        if let Some(child) = child.as_mut() {
            if let Some(pid) = pid {
                #[cfg(unix)]
                {
                    terminate_process_group(pid, libc::SIGTERM);
                }
            }
            let wait = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
            if wait.is_err() {
                if let Some(pid) = pid {
                    #[cfg(unix)]
                    {
                        terminate_process_group(pid, libc::SIGKILL);
                    }
                }
                let _ = child.kill().await;
            }
        }
        self.inner.pending.lock().await.clear();
        Ok(())
    }

    async fn restart(&self) -> Result<()> {
        self.stop().await?;
        self.start().await?;
        let mut state = self.inner.state.lock().await;
        state.initialized = false;
        Ok(())
    }

    async fn send_request(&self, request: JsonRpcRequest) -> Result<JsonRpcResponse> {
        if request.method != METHOD_INITIALIZE {
            self.ensure_initialized().await?;
        }
        let timeout_duration = Duration::from_secs(
            self.inner
                .config
                .request_timeout_seconds(&self.inner.defaults),
        );
        let result = self.send_raw_request(request, timeout_duration).await;
        if result.is_err()
            && matches!(
                self.inner.config.restart_policy(&self.inner.defaults),
                RestartPolicy::OnFailure | RestartPolicy::Always
            )
        {
            self.refresh_state().await;
        }
        result
    }

    async fn send_notification(&self, notification: JsonRpcNotification) -> Result<()> {
        if notification.method != METHOD_INITIALIZED {
            self.ensure_initialized().await?;
        }
        self.send_raw_notification(notification).await
    }

    async fn health(&self) -> BackendHealth {
        self.refresh_state().await;
        let state = self.inner.state.lock().await;
        let pid = state.pid;
        // RSS comes from the sampler-populated cache; never shell out from the
        // request hot path. If the metrics sampler hasn't ticked yet (or the
        // backend just started), `rss_kb` is `None` until the next sample
        // arrives.
        let rss_kb = self
            .inner
            .last_sample
            .lock()
            .as_ref()
            .and_then(|sample| sample.rss_kb);
        // Only report idle/req/pending when the backend is actually running —
        // a stopped backend has nothing meaningful to surface.
        let running = state.child.is_some();
        let last_used_seconds = running.then(|| state.last_used.elapsed().as_secs());
        let active_requests = running.then(|| state.active_requests as u64);
        let pending_requests = running.then(|| {
            self.inner
                .pending
                .try_lock()
                .map(|map| map.len())
                .unwrap_or(0) as u64
        });
        BackendHealth {
            name: self.inner.name.clone(),
            state: state.state.as_str().to_string(),
            pid,
            uptime_seconds: state.started_at.map(|started| started.elapsed().as_secs()),
            rss_kb,
            initialized: state.initialized,
            last_error: state.last_error.clone(),
            last_used_seconds,
            active_requests,
            pending_requests,
        }
    }

    fn notifications(&self) -> broadcast::Receiver<BackendNotification> {
        self.inner.notifications.subscribe()
    }

    fn rpc_metrics(&self) -> RpcMetricsSnapshot {
        self.inner.metrics.snapshot()
    }

    fn update_resource_sample(&self, sample: Option<ResourceSample>) {
        *self.inner.last_sample.lock() = sample;
    }

    fn last_resource_sample(&self) -> Option<ResourceSample> {
        *self.inner.last_sample.lock()
    }
}

fn browser_debug_url(config: &ServerConfig) -> Option<String> {
    config.args.iter().find_map(|arg| {
        arg.strip_prefix("--browser-url=")
            .map(|value| value.to_string())
    })
}

async fn ensure_browser_debug_url(url: &str) -> Result<()> {
    let version_url = format!("{}/json/version", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(1))
        .build()
        .map_err(|err| GatewayError::Backend(err.to_string()))?;
    let response = client.get(&version_url).send().await;
    match response {
        Ok(response) if response.status().is_success() => Ok(()),
        Ok(response) => Err(GatewayError::Backend(format!(
            "Browser remote debugging is not reachable at {url} (GET /json/version returned {})",
            response.status()
        ))),
        Err(err) => Err(GatewayError::Backend(format!(
            "Browser remote debugging is not reachable at {url}: {err}. Check the explicit browser attach endpoint, or remove the attach flag to let the MCP server launch its own isolated browser."
        ))),
    }
}

fn apply_backend_resource_policy(pid: u32) {
    #[cfg(target_os = "macos")]
    {
        if let Err(err) = try_apply_darwin_background(pid) {
            tracing::debug!(pid, error = %err, "failed to apply Darwin background policy");
            let _ = try_apply_nice(pid, 5);
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = try_apply_nice(pid, 5);
    }
}

#[cfg(target_os = "macos")]
fn try_apply_darwin_background(pid: u32) -> std::io::Result<()> {
    let rc = unsafe {
        libc::setpriority(
            libc::PRIO_DARWIN_PROCESS,
            pid as libc::id_t,
            libc::PRIO_DARWIN_BG,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn try_apply_nice(pid: u32, priority: libc::c_int) -> std::io::Result<()> {
    let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, priority) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn terminate_process_group(pid: u32, signal: libc::c_int) {
    let pgid = -(pid as libc::pid_t);
    unsafe {
        libc::kill(pgid, signal);
    }
}

async fn read_capped_line<R>(
    reader: &mut R,
    line: &mut Vec<u8>,
    max_bytes: usize,
) -> io::Result<Option<bool>>
where
    R: AsyncBufRead + Unpin,
{
    line.clear();
    let mut truncated = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            trim_line_end(line);
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(truncated))
            };
        }

        if let Some(pos) = available.iter().position(|byte| *byte == b'\n') {
            truncated |= append_capped(line, &available[..pos], max_bytes);
            reader.consume(pos + 1);
            trim_line_end(line);
            return Ok(Some(truncated));
        }

        let consumed = available.len();
        truncated |= append_capped(line, available, max_bytes);
        reader.consume(consumed);
    }
}

fn append_capped(line: &mut Vec<u8>, chunk: &[u8], max_bytes: usize) -> bool {
    let remaining = max_bytes.saturating_sub(line.len());
    let copied = remaining.min(chunk.len());
    if copied > 0 {
        line.extend_from_slice(&chunk[..copied]);
    }
    copied < chunk.len()
}

fn trim_line_end(line: &mut Vec<u8>) {
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_background_policy_can_be_applied_to_spawned_child() {
        use std::process::Command;

        let mut child = Command::new("/bin/sleep")
            .arg("1")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();

        let result = super::try_apply_darwin_background(pid);

        let _ = child.kill();
        let _ = child.wait();
        assert!(
            result.is_ok(),
            "Darwin background policy failed: {result:?}"
        );
    }
}
