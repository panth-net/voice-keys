#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_ID="voicekeys"
APP_NAME="Voice Keys"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/$APP_ID"
BIN_DIR="$HOME/.local/bin"
DESKTOP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/1024x1024/apps"
SOURCE_BIN="$SCRIPT_DIR/target/release/$APP_ID"
SOURCE_ICON="$SCRIPT_DIR/assets/icon.png"
TARGET_BIN="$APP_DIR/$APP_ID"
WRAPPER_BIN="$BIN_DIR/$APP_ID"
DESKTOP_FILE="$DESKTOP_DIR/$APP_ID.desktop"
TARGET_CONFIG="$APP_DIR/config.yaml"
SOURCE_CONFIG="$SCRIPT_DIR/target/release/config.yaml"

if [[ ! -x "$SOURCE_BIN" ]]; then
  echo "Release binary not found at $SOURCE_BIN"
  echo "Build it first with: cargo build --release"
  exit 1
fi

mkdir -p "$APP_DIR" "$BIN_DIR" "$DESKTOP_DIR" "$ICON_DIR"

install -m 0755 "$SOURCE_BIN" "$TARGET_BIN"
install -m 0644 "$SOURCE_ICON" "$ICON_DIR/$APP_ID.png"

if [[ ! -f "$TARGET_CONFIG" ]]; then
  if [[ -f "$SOURCE_CONFIG" ]]; then
    install -m 0644 "$SOURCE_CONFIG" "$TARGET_CONFIG"
  elif [[ -f "$SCRIPT_DIR/config.example.yaml" ]]; then
    install -m 0644 "$SCRIPT_DIR/config.example.yaml" "$TARGET_CONFIG"
  fi
fi

cat > "$WRAPPER_BIN" <<EOF
#!/usr/bin/env bash
exec "$TARGET_BIN" "\$@"
EOF
chmod 0755 "$WRAPPER_BIN"

cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Version=1.0
Type=Application
Name=$APP_NAME
Comment=Push-to-talk transcription tray app
Exec=$TARGET_BIN
Icon=$APP_ID
Terminal=false
Categories=Utility;AudioVideo;
Keywords=voice;dictation;transcription;tray;
StartupNotify=false
EOF

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$DESKTOP_DIR" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -q -t "${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Installed $APP_NAME"
echo "Binary: $TARGET_BIN"
echo "Launcher: $DESKTOP_FILE"
echo "CLI wrapper: $WRAPPER_BIN"
