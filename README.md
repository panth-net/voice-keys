# Voice Keys

Push-to-talk speech-to-text for macOS, Windows and Linux.

Hold a two-key hotkey, speak, press it again — the transcript is pasted straight
into whatever app you were typing in. Voice Keys lives in the tray/menu bar and
stays out of the way until you call it.

It has two modes, each with its own hotkey:

- **Paste** — transcribe and paste into the focused app.
- **Clipboard** — transcribe and copy to the clipboard.

Transcription is done by [Deepgram](https://deepgram.com); you bring your own
API key (the free tier is enough for everyday use).

## How the hotkeys work

Each mode is bound to exactly two keys:

1. Hold **Key 1**
2. Press **Key 2**

Recording starts. Press the same two-key combo again to stop and transcribe.
Recording also stops on its own at `audio.max_recording_minutes`.

## Privacy

Worth stating plainly, because Voice Keys asks for intrusive permissions:

- **Audio goes to Deepgram and nowhere else.** `api.deepgram.com` is the only
  host this app ever contacts. There is no telemetry, no analytics, no crash
  reporting, and no update check.
- **Voice Keys does not log your keystrokes.** It watches global key events to
  spot its own hotkeys and discards everything else. Key diagnostics exist but
  are off unless you set an environment variable — see
  [Debugging hotkeys](#debugging-hotkeys).
- **Transcripts are not written to disk** at the default log level. They go to
  your clipboard, and the last few are held in memory until the app exits.
- **Your API key is stored in plaintext** in `config.yaml`. See
  [SECURITY.md](SECURITY.md) for why, and what to do about it.

## Install

### macOS Gatekeeper

Release builds are ad-hoc signed and **not notarised**, so macOS will say the
app "cannot be opened because the developer cannot be verified". Either:

- Right-click the app → **Open** → **Open** in the dialog, or
- Clear the quarantine flag:
  ```bash
  xattr -dr com.apple.quarantine "/Applications/Voice Keys.app"
  ```

Building from source yourself avoids this entirely.

### Build from source

Requires **Rust 1.82 or newer** (the macOS event-tap module uses `&raw mut`).

**macOS** and **Windows** need no extra system packages.

**Linux (Debian/Ubuntu):**
```bash
sudo apt install build-essential pkg-config libssl-dev \
  libasound2-dev libx11-dev libxtst-dev libxdo-dev \
  libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  libnotify-bin
```

- `libasound2-dev` — microphone capture (ALSA)
- `libx11-dev` / `libxtst-dev` / `libxdo-dev` — global key handling and paste
- `libgtk-3-dev` / `libwebkit2gtk-4.1-dev` — the embedded settings window
- `libayatana-appindicator3-dev` — tray icon
- `libnotify-bin` — desktop notifications via `notify-send`

On Wayland the app may build and launch, but global hotkey capture can be
unreliable — the Linux input path is X11-oriented.

```bash
cargo build --release
```

Binary output:
- macOS/Linux: `target/release/voicekeys`
- Windows: `target/release/voicekeys.exe`

To build distributable packages (`.dmg`, Windows `.zip`, Linux tarball):
```bash
./build-release.sh
```

### Install into your Ubuntu app menu

```bash
chmod +x ./install-ubuntu.sh
./install-ubuntu.sh
```

Installs the binary, a `.desktop` launcher, the icon, and a CLI wrapper into
`~/.local`.

## First run

1. Launch `voicekeys`.
2. Click the tray/menu-bar icon to open settings.
3. Paste your Deepgram API key (get one at
   [console.deepgram.com](https://console.deepgram.com)) and pick a language.
4. Save.
5. Leave the app running in the tray/menu bar.

If `config.yaml` does not exist, the app creates one on first launch.

## Permissions

### macOS

1. Open **System Settings → Privacy & Security**.
2. Allow **Voice Keys** (or `voicekeys`) under **Accessibility**, **Input
   Monitoring**, and **Microphone**.
3. If you launched Voice Keys from Terminal, iTerm, or VS Code, grant that app
   the same permissions — macOS attributes them to the launching process.
4. Quit and reopen Voice Keys.

If hotkeys still don't fire, click out of any password field and try again.

### Windows 11

1. Open **Settings → Privacy & security → Microphone**.
2. Turn on **Microphone access**.
3. Turn on **Let desktop apps access your microphone**.
4. Quit and reopen Voice Keys.

No Accessibility or Input Monitoring permission is needed on Windows.

To keep the tray icon visible: **Settings → Personalization → Taskbar → Other
system tray icons** → toggle `voicekeys.exe` on.

## Configuration (`config.yaml`)

Everything here is editable from the settings UI; the file is just where it
lands.

```yaml
deepgram:
  api_key: ""
  model: "nova-3"
  language: "en"      # a single code like `en` or `pt-BR`, or `multi` for multilingual
  punctuate: true
  smart_format: true
  timeout_secs: 10.0

audio:
  sample_rate: 16000
  max_recording_minutes: 20

hotkeys:
  paste: ["alt", "dot"]
  clipboard: ["alt", "slash"]
```

Valid key names:

- Modifiers: `cmd`, `ctrl`, `alt`, `option`, `shift`
- Letters: `a` through `z`
- Numbers: `0` through `9`
- Symbols: `minus`, `equal`, `tilde`, `leftbracket`, `rightbracket`,
  `semicolon`, `quote`, `backslash`, `comma`, `dot` (or `period`), `slash`
- Other: `space`, `tab`, `enter`, `backspace`, `escape`, `capslock`, `f1`–`f12`

Config location:

| OS | Path |
|---|---|
| macOS / Linux | `~/.config/voicekeys/config.yaml` |
| Windows | `%APPDATA%\VoiceKeys\config.yaml` |

A `config.yaml` sitting next to the executable takes priority, for portable
installs.

## Start on boot

**macOS** — System Settings → General → Login Items → **+** → select the
`voicekeys` binary or `Voice Keys.app`.

**Windows** — press `Win+R`, run `shell:startup`, and put a shortcut to
`voicekeys.exe` in that folder. The bundled `installer/install.bat` can do this
for you.

**Linux** —
```bash
mkdir -p ~/.config/systemd/user
cat > ~/.config/systemd/user/voicekeys.service << 'EOF'
[Unit]
Description=Voice Keys

[Service]
ExecStart=%h/.local/share/voicekeys/voicekeys
Restart=on-failure

[Install]
WantedBy=default.target
EOF

systemctl --user enable --now voicekeys
```

## Troubleshooting

### No hotkey response on macOS

Almost always permissions. Confirm Voice Keys has **both** Accessibility and
Input Monitoring, and that the app you launched it *from* has them too. Quit and
relaunch after any permission change — macOS does not apply them to a running
process.

### Debugging hotkeys

Voice Keys does not record keystrokes. If you need a diagnostic, turn one on
explicitly:

```bash
VOICEKEYS_DEBUG_KEYS=1 ./target/release/voicekeys
```

Level `1` records the listener's lifecycle and the keys you have bound as
hotkeys. That is enough to tell a dead event tap from a mis-configured hotkey.

```bash
VOICEKEYS_DEBUG_KEYS=2 ./target/release/voicekeys
```

Level `2` **additionally records the first 200 raw key events, including keys
that are not your hotkeys.** Use it only when you are about to read the file
yourself, and delete it afterwards.

The file is written to your config directory, never to the current directory:

| OS | Path |
|---|---|
| macOS / Linux | `~/.config/voicekeys/debug_keys.log` |
| Windows | `%APPDATA%\VoiceKeys\debug_keys.log` |

```bash
tail -f ~/.config/voicekeys/debug_keys.log   # watch
rm ~/.config/voicekeys/debug_keys.log        # clean up
```

Healthy output starts with `CGEventTap created OK`, then `hotkey event: press
…` as you press your hotkeys. If you only ever see the startup lines, the
permissions aren't applied to the process you're actually running.

### No transcription

- Confirm the API key is saved in the settings UI.
- Check `voicekeys.log` for Deepgram request errors.

### No audio captured

Check the startup logs for the selected microphone device and sample rate.

### Verbose logging

```bash
RUST_LOG=debug ./target/release/voicekeys
```

Note that `debug` also logs a prefix of each transcript, which `info` does not.

## Contributing

Bug reports and pull requests are welcome — see
[CONTRIBUTING.md](CONTRIBUTING.md). Security issues should go through
[SECURITY.md](SECURITY.md) rather than a public issue.

## License

[MIT](LICENSE).

---

From [No pasa nada apps](https://nopasanada.app/), by
[Pantheon Network](https://pantheonnetwork.co/).
