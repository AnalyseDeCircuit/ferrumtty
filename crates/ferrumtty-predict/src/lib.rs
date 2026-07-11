// SPDX-License-Identifier: GPL-3.0-only

//! Conservative terminal-local prediction overlays.

const FAINT_RENDITION: &[u8] = b"\x1b[2m";
const NORMAL_INTENSITY: &[u8] = b"\x1b[22m";
const INSERT_CHARACTER: &[u8] = b"\x1b[@";
const ADAPTIVE_DISPLAY_MILLISECONDS: u16 = 20;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PredictionDisplay {
    #[default]
    Adaptive,
    Always,
    Never,
}

/// Identifies input provenance so uncertain multi-byte actions stay
/// ineligible even when their bytes contain printable characters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputKind {
    Key,
    Paste,
    Mouse,
    Focus,
}

/// Content-free counters suitable for diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PredictionMetrics {
    pub offered: u64,
    pub displayed: u64,
    pub latest_acknowledgement: Option<u64>,
    pub reconciled: u64,
}

/// Tracks only the number of tentative single-column cells. Authoritative
/// terminal or protocol state is never stored here.
pub struct PredictionOverlay {
    display: PredictionDisplay,
    overwrite: bool,
    round_trip_milliseconds: Option<u16>,
    pending_cells: usize,
    metrics: PredictionMetrics,
}

impl Default for PredictionOverlay {
    fn default() -> Self {
        Self::new(PredictionDisplay::Adaptive, false)
    }
}

impl PredictionOverlay {
    #[must_use]
    pub const fn new(display: PredictionDisplay, overwrite: bool) -> Self {
        Self {
            display,
            overwrite,
            round_trip_milliseconds: None,
            pending_cells: 0,
            metrics: PredictionMetrics {
                offered: 0,
                displayed: 0,
                latest_acknowledgement: None,
                reconciled: 0,
            },
        }
    }

    pub fn set_round_trip_milliseconds(&mut self, milliseconds: Option<u16>) {
        self.round_trip_milliseconds = milliseconds;
    }

    /// Returns terminal bytes for an eligible tentative key, or `None` when
    /// the input cannot be predicted conservatively.
    #[must_use]
    pub fn offer(&mut self, kind: InputKind, bytes: &[u8]) -> Option<Vec<u8>> {
        self.metrics.offered = self.metrics.offered.saturating_add(1);
        let display_enabled = match self.display {
            PredictionDisplay::Adaptive => self
                .round_trip_milliseconds
                .is_some_and(|milliseconds| milliseconds >= ADAPTIVE_DISPLAY_MILLISECONDS),
            PredictionDisplay::Always => true,
            PredictionDisplay::Never => false,
        };
        if !display_enabled
            || kind != InputKind::Key
            || bytes.len() != 1
            || !matches!(bytes[0], b' '..=b'~')
        {
            return None;
        }
        self.pending_cells = self.pending_cells.saturating_add(1);
        self.metrics.displayed = self.metrics.displayed.saturating_add(1);
        let mut output = Vec::with_capacity(bytes.len() + 16);
        if !self.overwrite {
            output.extend_from_slice(INSERT_CHARACTER);
        }
        output.extend_from_slice(FAINT_RENDITION);
        output.extend_from_slice(bytes);
        output.extend_from_slice(NORMAL_INTENSITY);
        Some(output)
    }

    /// Removes every tentative cell and restores the saved cursor before the
    /// caller renders authoritative output or processes a resize.
    #[must_use]
    pub fn reconcile(&mut self) -> bool {
        if self.pending_cells == 0 {
            return false;
        }
        let pending_cells = std::mem::take(&mut self.pending_cells);
        self.metrics.reconciled = self
            .metrics
            .reconciled
            .saturating_add(u64::try_from(pending_cells).unwrap_or(u64::MAX));
        true
    }

    /// Records the newest server `EchoAck` without treating it as terminal
    /// output. Rendering remains tentative until authoritative `HostBytes` arrive.
    pub fn acknowledge(&mut self, acknowledgement: u64) {
        self.metrics.latest_acknowledgement = Some(
            self.metrics
                .latest_acknowledgement
                .map_or(acknowledgement, |current| current.max(acknowledgement)),
        );
    }

    #[must_use]
    pub const fn metrics(&self) -> PredictionMetrics {
        self.metrics
    }
}

#[cfg(test)]
mod tests {
    use super::{InputKind, PredictionMetrics, PredictionOverlay};

    #[test]
    fn predicts_only_single_printable_ascii_keys() {
        let mut overlay = PredictionOverlay::new(super::PredictionDisplay::Always, true);
        assert_eq!(
            overlay.offer(InputKind::Key, b"a"),
            Some(b"\x1b[2ma\x1b[22m".to_vec())
        );
        assert!(overlay.offer(InputKind::Key, "界".as_bytes()).is_none());
        assert!(overlay.offer(InputKind::Key, b"\r").is_none());
        assert!(overlay.offer(InputKind::Paste, b"b").is_none());
        assert!(overlay.offer(InputKind::Mouse, b"c").is_none());
        assert!(overlay.offer(InputKind::Focus, b"d").is_none());
    }

    #[test]
    fn reconciliation_clears_overlay_before_authoritative_output() {
        let mut overlay = PredictionOverlay::new(super::PredictionDisplay::Always, true);
        let _ = overlay.offer(InputKind::Key, b"a");
        assert_eq!(
            overlay.offer(InputKind::Key, b"b"),
            Some(b"\x1b[2mb\x1b[22m".to_vec())
        );
        assert!(overlay.reconcile());
        assert!(!overlay.reconcile());
        assert_eq!(
            overlay.metrics(),
            PredictionMetrics {
                offered: 2,
                displayed: 2,
                latest_acknowledgement: None,
                reconciled: 2,
            }
        );
    }

    #[test]
    fn prediction_acknowledgements_are_monotonic() {
        let mut overlay = PredictionOverlay::default();
        overlay.acknowledge(9);
        overlay.acknowledge(4);
        assert_eq!(overlay.metrics().latest_acknowledgement, Some(9));
    }

    #[test]
    fn prediction_display_and_overwrite_settings_are_applied() {
        let mut never = PredictionOverlay::new(super::PredictionDisplay::Never, false);
        assert!(never.offer(InputKind::Key, b"a").is_none());

        let mut adaptive = PredictionOverlay::default();
        adaptive.set_round_trip_milliseconds(Some(19));
        assert!(adaptive.offer(InputKind::Key, b"a").is_none());
        adaptive.set_round_trip_milliseconds(Some(20));
        assert_eq!(
            adaptive.offer(InputKind::Key, b"a"),
            Some(b"\x1b[@\x1b[2ma\x1b[22m".to_vec())
        );
    }
}
