use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::backend::BackendNotification;
use crate::config::Config;
use crate::error::{GatewayError, Result};
use crate::jsonrpc::{JsonRpcNotification, METHOD_INITIALIZED};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeSession {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub target: String,
    pub group: Option<String>,
    pub server: Option<String>,
    pub client: Option<String>,
    pub initialized: bool,
    pub age_seconds: u64,
}

#[derive(Debug)]
struct SessionState {
    target: SessionTarget,
    client: Option<String>,
    created_at: Instant,
    last_seen: Instant,
    initialized: bool,
    queue: VecDeque<JsonRpcNotification>,
    /// Per-session waker. Replaces the single shared `Notify` so queueing a
    /// notification only wakes the one client that's actually receiving it —
    /// O(1) instead of O(N) under load.
    notify: Arc<Notify>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTarget {
    Group(String),
    Server(String),
}

impl From<String> for SessionTarget {
    fn from(group: String) -> Self {
        Self::Group(group)
    }
}

impl From<&str> for SessionTarget {
    fn from(group: &str) -> Self {
        Self::Group(group.to_string())
    }
}

#[derive(Clone)]
pub struct SessionManager {
    config: Arc<Config>,
    inner: Arc<Mutex<BTreeMap<String, SessionState>>>,
}

impl SessionManager {
    pub fn new(config: impl Into<Arc<Config>>) -> Self {
        let config = config.into();
        let manager = Self {
            config,
            inner: Arc::new(Mutex::new(BTreeMap::new())),
        };
        manager.spawn_session_reaper();
        manager
    }

    pub fn create(&self, target: impl Into<SessionTarget>) -> Result<BridgeSession> {
        self.create_with_client(target, None)
    }

    pub fn create_with_client(
        &self,
        target: impl Into<SessionTarget>,
        client: Option<String>,
    ) -> Result<BridgeSession> {
        let target = target.into();
        match &target {
            SessionTarget::Group(group) if !self.config.groups.contains_key(group) => {
                return Err(GatewayError::NotFound(format!("group '{group}'")));
            }
            SessionTarget::Server(server) if !self.config.servers.contains_key(server) => {
                return Err(GatewayError::NotFound(format!("server '{server}'")));
            }
            _ => {}
        }
        let mut sessions = self.inner.lock();
        if sessions.len() >= self.config.limits.max_clients {
            return Err(GatewayError::Config(format!(
                "max clients limit {} reached",
                self.config.limits.max_clients
            )));
        }
        let id = Uuid::new_v4().to_string();
        let now = Instant::now();
        sessions.insert(
            id.clone(),
            SessionState {
                target: target.clone(),
                client: client.clone(),
                created_at: now,
                last_seen: now,
                initialized: false,
                queue: VecDeque::new(),
                notify: Arc::new(Notify::new()),
            },
        );
        Ok(match target {
            SessionTarget::Group(group) => BridgeSession {
                id,
                group: Some(group),
                server: None,
                client,
            },
            SessionTarget::Server(server) => BridgeSession {
                id,
                group: None,
                server: Some(server),
                client,
            },
        })
    }

    /// Snapshot of every active session, optionally filtered to those that
    /// touch a particular backend (either directly or via a group).
    pub fn list(&self, server_filter: Option<&str>) -> Vec<SessionSummary> {
        let sessions = self.inner.lock();
        let now = Instant::now();
        sessions
            .iter()
            .filter(|(_, state)| match server_filter {
                None => true,
                Some(name) => self.target_includes_server(&state.target, name),
            })
            .map(|(id, state)| {
                let (group, server) = match &state.target {
                    SessionTarget::Group(group) => (Some(group.clone()), None),
                    SessionTarget::Server(server) => (None, Some(server.clone())),
                };
                let target = match &state.target {
                    SessionTarget::Group(g) => format!("group:{g}"),
                    SessionTarget::Server(s) => format!("server:{s}"),
                };
                SessionSummary {
                    id: id.clone(),
                    target,
                    group,
                    server,
                    client: state.client.clone(),
                    initialized: state.initialized,
                    age_seconds: now.duration_since(state.created_at).as_secs(),
                }
            })
            .collect()
    }

    pub fn group_for_session(&self, id: &str) -> Result<String> {
        match self.target_for_session(id)? {
            SessionTarget::Group(group) => Ok(group),
            SessionTarget::Server(server) => Err(GatewayError::NotFound(format!(
                "session '{id}' targets server '{server}', not a group"
            ))),
        }
    }

    pub fn target_for_session(&self, id: &str) -> Result<SessionTarget> {
        let mut sessions = self.inner.lock();
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| GatewayError::NotFound(format!("session '{id}'")))?;
        session.last_seen = Instant::now();
        Ok(session.target.clone())
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let removed = self.inner.lock().remove(id);
        if removed.is_some() {
            Ok(())
        } else {
            Err(GatewayError::NotFound(format!("session '{id}'")))
        }
    }

    pub fn record_client_notification(
        &self,
        id: &str,
        notification: JsonRpcNotification,
    ) -> Result<()> {
        let mut sessions = self.inner.lock();
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| GatewayError::NotFound(format!("session '{id}'")))?;
        session.last_seen = Instant::now();
        if notification.method == METHOD_INITIALIZED {
            session.initialized = true;
        }
        Ok(())
    }

    pub fn is_initialized(&self, id: &str) -> bool {
        self.inner
            .lock()
            .get(id)
            .map(|session| session.initialized)
            .unwrap_or(false)
    }

    pub async fn poll(&self, id: &str, timeout: Duration) -> Result<Vec<JsonRpcNotification>> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Grab the per-session notify under the lock so we don't miss a
            // notification that races between draining the queue and awaiting.
            let notify = {
                let mut sessions = self.inner.lock();
                let session = sessions
                    .get_mut(id)
                    .ok_or_else(|| GatewayError::NotFound(format!("session '{id}'")))?;
                session.last_seen = Instant::now();
                if !session.queue.is_empty() {
                    return Ok(session.queue.drain(..).collect());
                }
                session.notify.clone()
            };

            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(Vec::new());
            }
            let remaining = deadline - now;
            let _ = tokio::time::timeout(remaining, notify.notified()).await;
        }
    }

    pub fn start_notification_forwarder(
        &self,
        mut receiver: broadcast::Receiver<BackendNotification>,
    ) {
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(notification) => manager.queue_backend_notification(notification),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    fn queue_backend_notification(&self, notification: BackendNotification) {
        // Collect Arc<Notify> handles inside the lock, then wake them after
        // dropping the guard. Each affected session is woken individually — no
        // global broadcast — so polling clients that don't subscribe to this
        // backend never wake up spuriously.
        let mut to_wake: Vec<Arc<Notify>> = Vec::new();
        {
            let mut sessions = self.inner.lock();
            for session in sessions.values_mut() {
                if self.target_includes_server(&session.target, &notification.server) {
                    if session.queue.len() == self.config.limits.max_session_events {
                        session.queue.pop_front();
                    }
                    session.queue.push_back(notification.notification.clone());
                    to_wake.push(session.notify.clone());
                }
            }
        }
        for notify in to_wake {
            notify.notify_waiters();
        }
    }

    fn target_includes_server(&self, target: &SessionTarget, server: &str) -> bool {
        match target {
            SessionTarget::Server(target_server) => target_server == server,
            SessionTarget::Group(group) => self
                .config
                .groups
                .get(group)
                .map(|group| group.servers.iter().any(|name| name == server))
                .unwrap_or(false),
        }
    }

    fn spawn_session_reaper(&self) {
        let manager = self.clone();
        let ttl = Duration::from_secs(manager.config.limits.session_ttl_seconds);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                loop {
                    tokio::time::sleep(ttl).await;
                    let now = Instant::now();
                    manager
                        .inner
                        .lock()
                        .retain(|_, session| now.duration_since(session.last_seen) < ttl);
                }
            });
        }
    }
}
