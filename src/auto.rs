use std::time::Instant;

use crate::{Config, ModeSpec};

/// Shared game-active/idle state machine used by both the CLI `watch` loop
/// and the tray `tick`: tracks the last time a game was seen plus a grace
/// window, and detects active-state changes.
pub(crate) struct AutoSwitcher {
    pub(crate) last_seen: Option<Instant>,
    pub(crate) last_active: Option<bool>,
}

impl AutoSwitcher {
    pub(crate) fn new() -> Self {
        Self {
            last_seen: None,
            last_active: None,
        }
    }

    /// Feed the current game-active state; returns Some((spec, is_active,
    /// state_changed, reason)) when a target should be considered, None
    /// while inside the grace window with no game running.
    pub(crate) fn step(&mut self, cfg: &Config, active: bool) -> Option<(ModeSpec, bool, bool, &'static str)> {
        if active {
            self.last_seen = Some(Instant::now());
        }
        let (is_active, reason) = if active {
            (true, "game running")
        } else if self
            .last_seen
            .map(|t| t.elapsed().as_secs() >= cfg.grace_secs)
            .unwrap_or(false)
        {
            self.last_seen = None;
            (false, "idle")
        } else {
            return None;
        };
        let spec = cfg.spec_for(is_active);
        let state_changed = self.last_active != Some(is_active);
        Some((spec, is_active, state_changed, reason))
    }
}
