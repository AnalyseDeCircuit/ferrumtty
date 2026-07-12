// SPDX-License-Identifier: GPL-3.0-only

//! Content-safe command-line protocol diagnostics.

use crate::cli::ClientConfig;
use ferrumtty_predict::PredictionDisplay;
use ferrumtty_runtime::{ConnectionState, DiagnosticEvent};

/// Formats only closed, content-free diagnostic events.
pub(crate) struct DiagnosticLogger {
    verbosity: u8,
}

impl DiagnosticLogger {
    pub(crate) const fn new(verbosity: u8) -> Self {
        Self { verbosity }
    }

    pub(crate) fn write_startup(&self, config: &ClientConfig) {
        if self.verbosity == 0 {
            return;
        }
        eprintln!("ferrumtty: connection state=starting");
        if self.verbosity >= 2 {
            eprintln!(
                "ferrumtty: config escape={} title_prefix={} prediction={} overwrite={}",
                config.escape_byte,
                !config.title_no_prefix,
                prediction_name(config.prediction_display),
                config.prediction_overwrite
            );
        }
        if self.verbosity >= 3 {
            eprintln!("ferrumtty: diagnostics mode=metadata-only payload_logging=false");
        }
    }

    pub(crate) fn write_event(&self, event: DiagnosticEvent) {
        if let Some(message) = self.format_event(event) {
            eprintln!("{message}");
        }
    }

    #[must_use]
    pub(crate) fn format_event(&self, event: DiagnosticEvent) -> Option<String> {
        match event {
            DiagnosticEvent::ConnectionStateChanged { state } if self.verbosity >= 1 => {
                Some(format!(
                    "ferrumtty: connection state={}",
                    connection_state_name(state)
                ))
            }
            DiagnosticEvent::FreshUpdatePrepared {
                state_id,
                datagram_count,
                datagram_bytes,
                instruction_count,
                input_bytes,
            } if self.verbosity >= 2 => {
                let mut message = format!("ferrumtty: send kind=fresh state={state_id}");
                if self.verbosity >= 3 {
                    message.push_str(&format!(
                        " datagrams={datagram_count} bytes={datagram_bytes} instructions={instruction_count} input_bytes={input_bytes}"
                    ));
                }
                Some(message)
            }
            DiagnosticEvent::RetransmissionPrepared {
                state_id,
                datagram_count,
                datagram_bytes,
                retransmit_delay_milliseconds,
            } if self.verbosity >= 2 => {
                let mut message = format!(
                    "ferrumtty: send kind=retransmission state={state_id} delay_ms={retransmit_delay_milliseconds}"
                );
                if self.verbosity >= 3 {
                    message.push_str(&format!(
                        " datagrams={datagram_count} bytes={datagram_bytes}"
                    ));
                }
                Some(message)
            }
            DiagnosticEvent::InboundUpdateAccepted {
                packet_counter,
                base_state,
                target_state,
                acknowledged_state,
                discard_before,
                delta_bytes,
                advances_remote_state,
            } if self.verbosity >= 2 => {
                let mut message = format!(
                    "ferrumtty: receive old={base_state} new={target_state} ack={acknowledged_state} throwaway={discard_before} advanced={advances_remote_state}"
                );
                if self.verbosity >= 3 {
                    message.push_str(&format!(
                        " packet_counter={packet_counter} delta_bytes={delta_bytes}"
                    ));
                }
                Some(message)
            }
            DiagnosticEvent::RoundTripUpdated { milliseconds } if self.verbosity >= 2 => {
                Some(format!("ferrumtty: rtt milliseconds={milliseconds}"))
            }
            DiagnosticEvent::SessionLifecycleChanged { state } if self.verbosity >= 1 => {
                Some(format!("ferrumtty: session lifecycle={state:?}"))
            }
            DiagnosticEvent::UdpBindingChanged { generation } if self.verbosity >= 1 => {
                Some(format!("ferrumtty: udp binding_generation={generation}"))
            }
            DiagnosticEvent::ShutdownStarted if self.verbosity >= 1 => {
                Some("ferrumtty: shutdown state=started".to_owned())
            }
            DiagnosticEvent::ShutdownComplete { outcome } if self.verbosity >= 1 => Some(format!(
                "ferrumtty: shutdown state=complete outcome={outcome:?}"
            )),
            _ => None,
        }
    }
}

const fn connection_state_name(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Connecting => "connecting",
        ConnectionState::Connected => "connected",
        ConnectionState::Interrupted => "interrupted",
    }
}

const fn prediction_name(display: PredictionDisplay) -> &'static str {
    match display {
        PredictionDisplay::Adaptive => "adaptive",
        PredictionDisplay::Always => "always",
        PredictionDisplay::Never => "never",
    }
}

#[cfg(test)]
mod tests {
    use super::DiagnosticLogger;
    use ferrumtty_runtime::{ConnectionState, DiagnosticEvent};

    #[test]
    fn verbosity_levels_add_metadata_without_payloads() {
        let event = DiagnosticEvent::InboundUpdateAccepted {
            packet_counter: 7,
            base_state: 3,
            target_state: 4,
            acknowledged_state: 2,
            discard_before: 1,
            delta_bytes: 99,
            advances_remote_state: true,
        };
        assert_eq!(DiagnosticLogger::new(1).format_event(event), None);
        let level_two = DiagnosticLogger::new(2)
            .format_event(event)
            .expect("level two must report state metadata");
        assert!(!level_two.contains("packet_counter"));
        assert!(!level_two.contains("delta_bytes"));
        let level_three = DiagnosticLogger::new(3)
            .format_event(event)
            .expect("level three must report aggregate sizes");
        assert!(level_three.contains("packet_counter=7"));
        assert!(level_three.contains("delta_bytes=99"));
    }

    #[test]
    fn connection_events_are_visible_at_level_one() {
        assert_eq!(
            DiagnosticLogger::new(1).format_event(DiagnosticEvent::ConnectionStateChanged {
                state: ConnectionState::Interrupted,
            }),
            Some("ferrumtty: connection state=interrupted".to_owned())
        );
    }
}
