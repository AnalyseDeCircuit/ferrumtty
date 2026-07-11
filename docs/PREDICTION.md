# Conservative prediction policy

FerrumTTY prediction is a terminal-local overlay. It cannot mutate protocol
state, acknowledged state, the authoritative terminal model, or outgoing input.

Eligibility is intentionally narrow:

- exactly one printable ASCII byte from a key event is eligible;
- UTF-8 multibyte characters, combining characters, controls, escape
  sequences, function keys, mouse, focus, and all paste events are ineligible;
- eligible characters are rendered with faint rendition as an explicit visual
  indication;
- resize, authoritative server output, termination, and any error reconcile
  all pending predictions first.

The local console maintains a VT model fed only by authenticated authoritative
server bytes. Reconciliation clears and redraws the complete authoritative
screen from that model, including cursor position and cell rendition. This is
deliberately more expensive than guessing which cells changed, but prevents a
misprediction from leaving stale or erased cells.

Diagnostics expose only counts of offered, displayed, and reconciled actions.
They never contain input bytes or terminal content. Prediction is not used for
large paste or uncertain terminal modes.
