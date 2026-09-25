//! Cross-channel punch-event dedup.
//!
//! The card clock keeps a single event queue (PRD §2.9): the 8031 push (only
//! fires on power-on/reboot on the live unit) and the 25H/37H pull can surface
//! the *same* event twice. Each ingress path builds a fresh `GcpPunchEvent`
//! with its own `event_id`, so GCP-side idempotency cannot tell them apart.
//! This guard drops a punch if an identical (occurred_at, card, event) was seen
//! within a sliding window.

use std::collections::VecDeque;

/// Sliding-window dedup keyed on (Unix time second, normalized UID, event code).
#[derive(Debug)]
pub struct PunchDedup {
    window_secs: i64,
    /// (occurred_at unix sec, uid_hex, event_code); kept in oldest-first order.
    seen: VecDeque<(i64, String, String)>,
}

impl PunchDedup {
    pub fn new(window_secs: i64) -> Self {
        Self {
            window_secs: window_secs.max(0),
            seen: VecDeque::new(),
        }
    }

    /// Returns `true` if this punch should be suppressed (already seen inside the window).
    pub fn is_duplicate(&mut self, occurred_at_unix: i64, uid_hex: &str, event_code: &str) -> bool {
        let uid = uid_hex.trim();
        if uid.is_empty() {
            // System events without a card UID (e.g. M24 power-on) are not punch
            // candidates and are not deduped.
            return false;
        }
        let limit = occurred_at_unix - self.window_secs;
        while let Some(&(t, _, _)) = self.seen.front() {
            if t < limit {
                self.seen.pop_front();
            } else {
                break;
            }
        }
        let key = (occurred_at_unix, uid.to_string(), event_code.to_string());
        if self.seen.iter().any(|(t, u, e)| {
            (*t).abs_diff(occurred_at_unix) <= 0 && u == uid && e == event_code
        }) {
            return true;
        }
        self.seen.push_back(key);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedups_identical_punch_inside_window() {
        let mut d = PunchDedup::new(600);
        assert!(!d.is_duplicate(1_700_000_000, "00000000FD6374F6", "M11"));
        assert!(d.is_duplicate(1_700_000_000, "00000000FD6374F6", "M11"));
    }

    #[test]
    fn allows_different_card_or_event() {
        let mut d = PunchDedup::new(600);
        assert!(!d.is_duplicate(1_700_000_000, "00000000FD6374F6", "M11"));
        assert!(!d.is_duplicate(1_700_000_000, "00000000FD6374F6", "M03"));
        assert!(!d.is_duplicate(1_700_000_000, "00000000FD6374F5", "M11"));
    }

    #[test]
    fn expires_after_window() {
        let mut d = PunchDedup::new(600);
        let t = 1_700_000_000;
        assert!(!d.is_duplicate(t, "00000000FD6374F6", "M11"));
        // 601 s later: outside the 600 s window -> allowed again
        assert!(!d.is_duplicate(t + 601, "00000000FD6374F6", "M11"));
    }

    #[test]
    fn ignores_system_events_without_uid() {
        let mut d = PunchDedup::new(600);
        assert!(!d.is_duplicate(1_700_000_000, "", "M24"));
        assert!(!d.is_duplicate(1_700_000_000, "  ", "M24"));
    }
}