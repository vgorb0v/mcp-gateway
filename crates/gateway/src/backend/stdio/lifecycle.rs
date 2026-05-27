#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LifecycleState {
    Stopped,
    Starting,
    Running,
    Unhealthy,
}

impl LifecycleState {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            LifecycleState::Stopped => "stopped",
            LifecycleState::Starting => "starting",
            LifecycleState::Running => "running",
            LifecycleState::Unhealthy => "unhealthy",
        }
    }
}
