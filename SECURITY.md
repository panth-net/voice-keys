# Security Policy

## Reporting a vulnerability

Please do not open a public issue for security reports.

Report privately via <https://www.pantheonnetwork.co/contact> with "Voice Keys
security" in the subject. Include the version, your OS, reproduction steps, and
the impact you believe it has.

We aim to acknowledge within 3 business days and to ship a fix or a documented
mitigation within 90 days of acknowledgement. We'll credit you in the changelog
unless you'd rather we didn't. There is no bug bounty.

## Supported versions

Only the latest release on `main` receives security fixes.

## What Voice Keys does with your data

- Recorded audio is sent to Deepgram (`api.deepgram.com`) for transcription and
  nowhere else. See Deepgram's privacy policy for their retention terms.
- Transcripts are placed on your system clipboard, and the most recent ones are
  held in memory until the app exits. They are not written to disk at the
  default log level.
- There is no telemetry, analytics, crash reporting, or update check.
- Voice Keys does not record keystrokes. It observes global key events in order
  to detect its own hotkeys and discards everything else. Opt-in key
  diagnostics exist; see "Debugging hotkeys" in the README.

## Known limitations

These are deliberate tradeoffs, documented rather than hidden.

- **The Deepgram API key is stored in plaintext** in `config.yaml`
  (`~/.config/voicekeys/config.yaml`, or `%APPDATA%\VoiceKeys\config.yaml`). It
  is not encrypted and is not in the OS keychain. Protect that file
  accordingly, and prefer a scoped Deepgram key over an account-wide one.
- On macOS, Voice Keys requires **Accessibility** and **Input Monitoring** to
  detect global hotkeys and to synthesise the paste keystroke. These are broad
  permissions. The event tap is created listen-only
  (`kCGEventTapOptionListenOnly`), so Voice Keys cannot modify, inject into, or
  swallow your keystrokes — only observe them.
- Release builds are ad-hoc code-signed and **not notarised**.
- `voicekeys.log` may contain your microphone device name and Deepgram error
  responses. Skim it before attaching it to a bug report.
- Running with `RUST_LOG=debug` or `VOICEKEYS_DEBUG_KEYS=2` writes data to disk
  that the defaults deliberately do not — a transcript prefix and raw key
  events respectively. Both are opt-in, and both files are worth deleting once
  you're done with them.
