# Contributing

Voice Keys is a free app from [No pasa nada apps](https://nopasanada.app/),
built by [Pantheon Network](https://pantheonnetwork.co/). It began as an
internal tool for our own team because we were just so wowed by the quality of Deepgram's transcription quality, and so we decided to release it publicly. We hope it helps you as much as it helps us!

It is no longer under active development.

## Security issues

We try to stay on top of our public email inbox and will respond to security issues promptly. Report
them via [pantheonnetwork.co/contact](https://www.pantheonnetwork.co/contact)
rather than GitHub issues.

## Other contributions

We are not developing new features or fixing non-security bugs, and do not
actively monitor GitHub issues or pull requests. If you would like to
maintain this project going forward, or fork it, you are welcome to do so. Please feel free to reach out to us via [pantheonnetwork.co/contact](https://www.pantheonnetwork.co/contact) if you have any questions.

## Setup

Requires [Rust](https://rustup.rs) 1.82 or later.

macOS and Windows require no additional setup. On Ubuntu or Debian:

```bash
sudo apt install build-essential pkg-config libssl-dev libasound2-dev \
  libx11-dev libxtst-dev libxdo-dev libgtk-3-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev libnotify-bin
```

```bash
cargo run                       # run the app
RUST_LOG=debug cargo run        # run with verbose logging
```

> **`config.yaml` contains a live API key.** It is gitignored, but Voice
> Keys reads a `config.yaml` in the project directory before the one in your
> home directory, so running `cargo run` from a checkout can create one
> there. Do not commit it.

## Commits and licensing

Contributions are submitted under the MIT license — see
[LICENSE](LICENSE).
