//! The extension state machine and its crash-restart policy.
//!
//! Both are pure functions of an injected instant so the rate limiter can be
//! tested without sleeping and without a real child process.
use std::time::Duration;

use instant::Instant;

/// Failed starts tolerated inside [`RestartPolicy::window`] before giving up.
pub const DEFAULT_MAX_FAILURES: usize = 3;
/// Window over which failures are counted.
pub const DEFAULT_FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionState {
    Discovered,
    /// The manifest or executable is unusable; the reason is shown to the user.
    Invalid {
        reason: String,
    },
    Inactive,
    Starting,
    Running,
    Stopping,
    /// Failed too often to keep restarting automatically.
    Failed {
        reason: String,
    },
}

impl ExtensionState {
    pub fn is_running(&self) -> bool {
        matches!(self, ExtensionState::Running)
    }

    /// True when Warp will not start this extension again without the user
    /// asking, so the UI can offer a manual restart instead of waiting.
    pub fn needs_user_action(&self) -> bool {
        matches!(
            self,
            ExtensionState::Invalid { .. } | ExtensionState::Failed { .. }
        )
    }
}

/// Why a running extension stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// Warp asked it to stop: deactivation, disable, or shutdown.
    Requested,
    /// The activation condition no longer holds.
    Deactivated,
    Crashed {
        detail: String,
    },
    /// The handshake did not complete inside the start timeout.
    StartTimeout,
    /// The plugin misbehaved badly enough to be disconnected.
    ProtocolViolation {
        detail: String,
    },
}

impl StopReason {
    /// Only unexpected stops count towards the restart budget; a deliberate
    /// stop must not push an extension towards `Failed`.
    pub fn is_failure(&self) -> bool {
        match self {
            StopReason::Requested | StopReason::Deactivated => false,
            StopReason::Crashed { .. }
            | StopReason::StartTimeout
            | StopReason::ProtocolViolation { .. } => true,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            StopReason::Requested => "stopped by Warp".to_owned(),
            StopReason::Deactivated => "workspace no longer matches activation".to_owned(),
            StopReason::Crashed { detail } => detail.clone(),
            StopReason::StartTimeout => "did not complete the handshake in time".to_owned(),
            StopReason::ProtocolViolation { detail } => detail.clone(),
        }
    }
}

/// What the host should do after a stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartDecision {
    /// Restart now, offering the user a notification naming the extension.
    Restart,
    /// Stay stopped until the activation condition holds again.
    StayInactive,
    /// Out of restart budget; only a manual restart will start it again.
    GiveUp { reason: String },
}

#[derive(Debug, Clone, Copy)]
pub struct RestartPolicy {
    pub max_failures: usize,
    pub window: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            max_failures: DEFAULT_MAX_FAILURES,
            window: DEFAULT_FAILURE_WINDOW,
        }
    }
}

/// Tracks one extension's state and its recent failures.
#[derive(Debug, Clone)]
pub struct StateMachine {
    state: ExtensionState,
    policy: RestartPolicy,
    failures: Vec<Instant>,
}

impl StateMachine {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            state: ExtensionState::Discovered,
            policy,
            failures: Vec::new(),
        }
    }

    pub fn state(&self) -> &ExtensionState {
        &self.state
    }

    /// Records that validation failed. An invalid extension is never started,
    /// so this is terminal until the files on disk change.
    pub fn invalidate(&mut self, reason: impl Into<String>) {
        self.state = ExtensionState::Invalid {
            reason: reason.into(),
        };
    }

    /// Marks a validated extension as eligible but not yet activated.
    pub fn validated(&mut self) {
        if !self.state.needs_user_action() {
            self.state = ExtensionState::Inactive;
        }
    }

    /// Returns false when the extension must not be started right now, which is
    /// the case for an invalid manifest or an exhausted restart budget.
    pub fn start(&mut self) -> bool {
        if self.state.needs_user_action() {
            return false;
        }
        self.state = ExtensionState::Starting;
        true
    }

    /// The handshake completed.
    pub fn running(&mut self) {
        self.state = ExtensionState::Running;
    }

    pub fn stopping(&mut self) {
        self.state = ExtensionState::Stopping;
    }

    /// Clears the failure history after a start the user asked for, so a manual
    /// restart is a genuine fresh chance rather than an immediate `Failed`.
    ///
    /// An invalid manifest is not revived this way: nothing the user does in
    /// the UI can make an unparseable manifest runnable.
    pub fn manual_restart(&mut self) -> bool {
        if matches!(self.state, ExtensionState::Invalid { .. }) {
            return false;
        }
        self.failures.clear();
        self.state = ExtensionState::Inactive;
        self.start()
    }

    /// Applies a stop and reports what the host should do next.
    pub fn stopped(&mut self, reason: StopReason, now: Instant) -> RestartDecision {
        if !reason.is_failure() {
            self.state = ExtensionState::Inactive;
            return RestartDecision::StayInactive;
        }

        self.failures
            .retain(|failure| now.duration_since(*failure) < self.policy.window);
        self.failures.push(now);

        if self.failures.len() >= self.policy.max_failures {
            let reason = format!(
                "stopped {} times in {} seconds: {}",
                self.failures.len(),
                self.policy.window.as_secs(),
                reason.detail()
            );
            self.state = ExtensionState::Failed {
                reason: reason.clone(),
            };
            return RestartDecision::GiveUp { reason };
        }

        self.state = ExtensionState::Inactive;
        RestartDecision::Restart
    }
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
