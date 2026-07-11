// SPDX-License-Identifier: GPL-3.0-only

//! Conservative terminal-local prediction overlays.

const FAINT_RENDITION: &[u8] = b"\x1b[2m";
const NORMAL_INTENSITY: &[u8] = b"\x1b[22m";

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
    pub reconciled: u64,
}

/// Tracks only the number of tentative single-column cells. Authoritative
/// terminal or protocol state is never stored here.
#[derive(Default)]
pub struct PredictionOverlay {
    pending_cells: usize,
    metrics: PredictionMetrics,
}

impl PredictionOverlay {
    /// Returns terminal bytes for an eligible tentative key, or `None` when
    /// the input cannot be predicted conservatively.
    #[must_use]
    pub fn offer(&mut self, kind: InputKind, bytes: &[u8]) -> Option<Vec<u8>> {
        self.metrics.offered = self.metrics.offered.saturating_add(1);
        if kind != InputKind::Key || bytes.len() != 1 || !matches!(bytes[0], b' '..=b'~') {
            return None;
        }
        self.pending_cells = self.pending_cells.saturating_add(1);
        self.metrics.displayed = self.metrics.displayed.saturating_add(1);
        let mut output = Vec::with_capacity(bytes.len() + 13);
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
        let mut overlay = PredictionOverlay::default();
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
        let mut overlay = PredictionOverlay::default();
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
                reconciled: 2,
            }
        );
    }
}
