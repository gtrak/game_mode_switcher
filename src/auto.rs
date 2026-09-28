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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn same_spec(a: ModeSpec, b: ModeSpec) -> bool {
        a.w == b.w && a.h == b.h && a.hz == b.hz
    }

    #[test]
    fn new_starts_with_no_state() {
        let s = AutoSwitcher::new();
        assert!(s.last_seen.is_none());
        assert!(s.last_active.is_none());
    }

    #[test]
    fn active_step_returns_game_spec_and_change() {
        let cfg = crate::Config::default();
        let mut s = AutoSwitcher::new();
        let Some((sp, is_active, changed, reason)) = s.step(&cfg, true) else {
            panic!("expected Some for active=true");
        };
        assert!(same_spec(sp, cfg.auto_game));
        assert!(is_active);
        assert!(changed);
        assert_eq!(reason, "game running");
        assert!(s.last_seen.is_some());
    }

    #[test]
    fn inactive_within_grace_returns_none() {
        let cfg = crate::Config::default();
        let mut s = AutoSwitcher::new();
        s.step(&cfg, true);
        s.last_active = Some(true); // caller records the transition
        assert!(s.step(&cfg, false).is_none());
    }

    #[test]
    fn inactive_after_grace_returns_idle() {
        let cfg = crate::Config::default();
        let mut s = AutoSwitcher::new();
        s.step(&cfg, true);
        s.last_active = Some(true);
        s.last_seen = Some(Instant::now() - Duration::from_secs(cfg.grace_secs + 1));
        let Some((sp, is_active, changed, reason)) = s.step(&cfg, false) else {
            panic!("expected Some after grace expiry");
        };
        assert!(same_spec(sp, cfg.auto_idle));
        assert!(!is_active);
        assert!(changed);
        assert_eq!(reason, "idle");
        assert!(s.last_seen.is_none());
    }

    #[test]
    fn active_again_after_idle_reports_new_change() {
        let cfg = crate::Config::default();
        let mut s = AutoSwitcher::new();
        s.step(&cfg, true);
        s.last_active = Some(true);
        s.last_seen = Some(Instant::now() - Duration::from_secs(cfg.grace_secs + 1));
        assert!(s.step(&cfg, false).is_some());
        s.last_active = Some(false); // caller records the idle transition
        let Some((sp, is_active, changed, reason)) = s.step(&cfg, true) else {
            panic!("expected Some for active=true");
        };
        assert!(same_spec(sp, cfg.auto_game));
        assert!(is_active);
        assert!(changed);
        assert_eq!(reason, "game running");
    }
}
