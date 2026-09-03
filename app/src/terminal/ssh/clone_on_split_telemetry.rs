//! Telemetry for splitting a warpified SSH pane onto its source's connection.
//!
//! The events are split across two moments because no single moment knows the whole answer.
//! `PaneGroup` knows whether an attach was *requested*; only the new pane's own bootstrap knows
//! whether it worked, because the wrapper re-runs `ssh -O check` and fails closed on a master that
//! has gone away. Reporting success at request time would claim it in exactly the case this
//! feature exists for.

use serde_json::{Value, json};
use strum_macros::{EnumDiscriminants, EnumIter};
use warp_core::features::FeatureFlag;
use warp_core::telemetry::{EnablementState, TelemetryEvent, TelemetryEventDesc};

#[derive(Debug, EnumDiscriminants)]
#[strum_discriminants(derive(EnumIter))]
pub enum SshCloneOnSplitTelemetryEvent {
    /// An attach was asked for: every app-side gate passed and the new pane carries the socket.
    Requested,
    /// The user split a warpified SSH pane and got a local one, decided app-side before the
    /// wrapper ran. `reason` is `CloneDeclined::telemetry_reason`.
    Declined { reason: &'static str },
    /// The replayed `ssh` came up warpified in the new pane, on the source's connection.
    Succeeded,
    /// The replayed `ssh` finished without warpifying, so the wrapper refused the attach and
    /// handed the pane back at a local prompt.
    FellBackToLocal,
    /// The pane's shell exited while the attach attempt was still outstanding, so it neither
    /// cloned nor fell back. Reported rather than dropped so a success rate is not read as low
    /// merely because users close panes while connecting. It does not cover every such close:
    /// closing a pane outright delivers no `Exit`, and that attempt reports nothing.
    Abandoned,
}

impl TelemetryEvent for SshCloneOnSplitTelemetryEvent {
    fn name(&self) -> &'static str {
        SshCloneOnSplitTelemetryEventDiscriminants::from(self).name()
    }

    fn payload(&self) -> Option<Value> {
        match self {
            Self::Declined { reason } => Some(json!({ "reason": reason })),
            Self::Requested | Self::Succeeded | Self::FellBackToLocal | Self::Abandoned => None,
        }
    }

    fn description(&self) -> &'static str {
        SshCloneOnSplitTelemetryEventDiscriminants::from(self).description()
    }

    fn enablement_state(&self) -> EnablementState {
        SshCloneOnSplitTelemetryEventDiscriminants::from(self).enablement_state()
    }

    /// `reason` is one of a closed set of Warp-authored strings, and no event here carries a host
    /// name, a directory, or any part of the replayed command.
    fn contains_ugc(&self) -> bool {
        false
    }

    fn event_descs() -> impl Iterator<Item = Box<dyn TelemetryEventDesc>> {
        warp_core::telemetry::enum_events::<Self>()
    }
}

impl TelemetryEventDesc for SshCloneOnSplitTelemetryEventDiscriminants {
    fn name(&self) -> &'static str {
        match self {
            Self::Requested => "SshCloneOnSplit.Requested",
            Self::Declined => "SshCloneOnSplit.Declined",
            Self::Succeeded => "SshCloneOnSplit.Succeeded",
            Self::FellBackToLocal => "SshCloneOnSplit.FellBackToLocal",
            Self::Abandoned => "SshCloneOnSplit.Abandoned",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::Requested => {
                "User split a warpified SSH pane and Warp asked the new pane to join that \
                 connection"
            }
            Self::Declined => {
                "User split a warpified SSH pane and Warp opened a local pane instead, decided \
                 before the SSH wrapper ran"
            }
            Self::Succeeded => "A split pane came up on the source pane's SSH connection",
            Self::FellBackToLocal => {
                "A split pane asked to join its source's SSH connection and the wrapper refused, \
                 leaving a local pane"
            }
            Self::Abandoned => {
                "A split pane's shell exited while it was still connecting, so its attach attempt \
                 reached neither outcome"
            }
        }
    }

    fn enablement_state(&self) -> EnablementState {
        EnablementState::Flag(FeatureFlag::CloneSshOnSplit)
    }
}

warp_core::register_telemetry_event!(SshCloneOnSplitTelemetryEvent);
