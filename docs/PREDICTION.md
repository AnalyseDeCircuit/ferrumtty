# Conservative prediction policy

FerrumTTY prediction is a terminal-local overlay. It cannot mutate protocol
state, acknowledged state, the authoritative terminal model, or outgoing input.

Eligibility is intentionally narrow:

- consecutive single-scalar printable UTF-8 key events are eligible;
- Backspace may cancel only the last unacknowledged local printable prediction;
- unmodified left and right movement stays within the current predicted
  span;
- controls, escape sequences, function keys, mouse, focus, multi-scalar input,
  and all paste events are ineligible;
- eligible characters are rendered with underline rendition as an explicit
  visual indication;
- resize, authoritative server output, termination, and any error reconcile
  all pending predictions first.

The local console maintains a VT model fed only by authenticated authoritative
server bytes. Each eligible span is anchored to that model's real zero-based
row, column, width, cursor state, and rendition. Prediction stops before the
right margin and when the authoritative line identity changes. Insert-mode
spans are removed with a local delete-character operation on only that line;
overwrite-mode spans still redraw the complete authoritative screen because
the overwritten cells are intentionally not retained by the prediction crate.

Every tentative action and displayed cell is associated with the client SSP
frame that will carry it. Only a non-future, strictly advancing server
`EchoAck` can mark cells through that frame as acknowledged. A line is reported
as acknowledged only when all of its remaining predicted cells have explicit
cumulative acknowledgement. Pending predictions are bounded to 1024 entries.
Controls, resize, paste, mouse, focus, and unsupported edits act as barriers
and require authoritative reconciliation before prediction resumes.

Diagnostics expose only counts of offered, displayed, and reconciled actions.
They never contain input bytes or terminal content. FerrumTTY cannot observe a
remote program's termios `ECHO` flag, so it does not claim reliable automatic
password-field detection; unsupported or ambiguous input remains unpredicted.
