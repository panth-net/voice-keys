# Contributing to Voice Keys

Thanks for taking the time. Voice Keys is a free app from
[No pasa nada apps](https://nopasanada.app/), by
[Pantheon Network](https://pantheonnetwork.co/).

## Before you write code

Open an issue first for anything beyond a bug fix or a typo. The app is
deliberately small, and we'd rather say "yes, and here's where it goes" than
turn down a finished PR.

## Setup

Rust 1.82 or newer.

**macOS** and **Windows** need no extra system packages.

**Linux (Debian/Ubuntu):**
```bash
sudo apt install build-essential pkg-config libssl-dev libasound2-dev \
  libx11-dev libxtst-dev libxdo-dev libgtk-3-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev libnotify-bin
```

```bash
cargo run                       # debug build
RUST_LOG=debug cargo run        # verbose
```

> **Never commit `config.yaml`.** It holds a live API key. It's gitignored, but
> note that a `config.yaml` next to the executable takes priority for portable
> installs — during `cargo run` from the repo root, that means one can appear in
> your working tree. Don't `git add -f` it.

## Before you open a PR

These are exactly what CI runs:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

One test hits the live Deepgram usage API and is ignored by default:

```bash
VOICEKEYS_TEST_DG_KEY=<a Deepgram admin key> cargo test -- --ignored
```

## Code layout

The app is a single `src/main.rs`. That's intentional for now — if you're adding
a subsystem big enough to want its own module, say so in the issue.

- `mod macos_keys` is a hand-rolled `CGEventTap` binding, and the only `unsafe`
  in the codebase. Changes there need a `// SAFETY:` comment.
- `ui/index.html` and `ui/styles.css` are `include_str!`'d and injected into a
  `wry` webview via `with_html`. **There is no base URL**, so every asset must
  be inline or a `data:` URI — a relative `url()` or `<script src>` will
  silently fail to load. Don't add CDN references either; see below.

## Privacy bar

Voice Keys asks for microphone access and, on macOS, Input Monitoring. That
buys a duty of care. Pull requests will be closed if they:

- add telemetry, analytics, crash reporting, or update pings;
- add any network call to a host other than Deepgram, including webfont and
  script CDNs;
- log keystrokes or transcript text at the default log level.

Diagnostics are welcome, but they must be opt-in via an environment variable
and must write outside the working directory.

## Commits and licensing

Conventional-ish subject lines (`fix:`, `feat:`, `docs:`) are appreciated but
not enforced. By submitting a pull request you agree that your contribution is
licensed under the MIT License — see [LICENSE](LICENSE).
