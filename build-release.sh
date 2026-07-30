#!/usr/bin/env bash
set -euo pipefail

# Single source of truth: Cargo.toml. Keeps the bundle version from drifting
# away from the binary it wraps.
VERSION="$(grep -m1 '^version' "$(dirname "$0")/Cargo.toml" | cut -d'"' -f2)"
APP_NAME="Voice Keys"
BIN_NAME="voicekeys"
BUNDLE_ID="app.nopasanada.voicekeys"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
DIST_DIR="$SCRIPT_DIR/dist"

cd "$SCRIPT_DIR"

echo "==> Building release binary..."
# Rust bakes the source path of every panic location into the binary, which
# otherwise leaks the building machine's home directory (and username) into
# every artifact we ship. Rewrite those prefixes so published builds carry no
# trace of whoever compiled them.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME/.cargo=/cargo --remap-path-prefix=$SCRIPT_DIR=/voicekeys --remap-path-prefix=$HOME=/home"
cargo build --release

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

OS="$(uname -s)"
case "$OS" in
  Darwin)
    echo "==> Packaging macOS .app bundle..."
    APP_DIR="$DIST_DIR/$APP_NAME.app"
    mkdir -p "$APP_DIR/Contents/MacOS"
    mkdir -p "$APP_DIR/Contents/Resources"
    cp "target/release/$BIN_NAME" "$APP_DIR/Contents/MacOS/"
    cp "assets/icon.icns" "$APP_DIR/Contents/Resources/AppIcon.icns"
    cat > "$APP_DIR/Contents/Info.plist" << PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleExecutable</key>
    <string>${BIN_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Voice Keys needs microphone access for speech-to-text transcription.</string>
    <key>NSAppleEventsUsageDescription</key>
    <string>Voice Keys uses accessibility to paste transcriptions.</string>
</dict>
</plist>
PLIST

    # Clean extended attributes and ad-hoc code sign
    echo "==> Signing app bundle..."
    xattr -cr "$APP_DIR"
    # Ad-hoc signature only — this build is not notarised, so users will need to
    # clear the quarantine flag on first launch. See "macOS Gatekeeper" in README.
    codesign --force --deep --sign - --identifier "$BUNDLE_ID" "$APP_DIR"
    echo "==> Signed (ad-hoc) as $BUNDLE_ID"

    # Create DMG with drag-to-Applications installer
    echo "==> Creating DMG..."
    DMG_NAME="VoiceKeys-${VERSION}-macos.dmg"
    DMG_TEMP="$DIST_DIR/tmp.dmg"

    # Create a temporary DMG
    hdiutil create -size 10m -fs HFS+ -volname "$APP_NAME" -ov "$DMG_TEMP"

    # Mount it
    MOUNT_DIR=$(hdiutil attach "$DMG_TEMP" -readwrite -noverify | grep "/Volumes/" | awk '{print $NF}')
    MOUNT_POINT="/Volumes/$APP_NAME"

    # Copy .app into the DMG
    cp -R "$APP_DIR" "$MOUNT_POINT/"

    # Create symlink to Applications folder
    ln -s /Applications "$MOUNT_POINT/Applications"

    # Set icon positions and background via .DS_Store
    # Use AppleScript to arrange icons nicely
    osascript <<EOF
tell application "Finder"
    tell disk "$APP_NAME"
        open
        set current view of container window to icon view
        set toolbar visible of container window to false
        set statusbar visible of container window to false
        set bounds of container window to {400, 200, 900, 460}
        set theViewOptions to the icon view options of container window
        set arrangement of theViewOptions to not arranged
        set icon size of theViewOptions to 80
        set position of item "$APP_NAME.app" of container window to {120, 130}
        set position of item "Applications" of container window to {380, 130}
        close
    end tell
end tell
EOF

    # Unmount
    hdiutil detach "$MOUNT_POINT"

    # Convert to compressed, read-only DMG
    hdiutil convert "$DMG_TEMP" -format UDZO -o "$DIST_DIR/$DMG_NAME"
    rm -f "$DMG_TEMP"

    echo "==> Done: dist/$DMG_NAME"
    ;;

  MINGW*|MSYS*|CYGWIN*|Windows_NT)
    echo "==> Packaging Windows installer..."
    STAGING="$DIST_DIR/VoiceKeys"
    mkdir -p "$STAGING"
    cp "target/release/${BIN_NAME}.exe" "$STAGING/"
    cp "installer/install.bat" "$STAGING/"
    cp "assets/icon.ico" "$STAGING/" 2>/dev/null || true
    cp "installer/README.txt" "$STAGING/"
    # Create a zip — convert MSYS paths to Windows paths for PowerShell
    WIN_STAGING="$(cygpath -w "$STAGING")"
    WIN_DIST_DIR="$(cygpath -w "$DIST_DIR")"
    if command -v powershell.exe &>/dev/null; then
      powershell.exe -NoProfile -Command \
        "Compress-Archive -Path '${WIN_STAGING}' -DestinationPath '${WIN_DIST_DIR}\\VoiceKeys-${VERSION}-windows.zip' -Force"
      echo "==> Done: dist/VoiceKeys-${VERSION}-windows.zip"
    else
      echo "==> Done: dist/VoiceKeys/ (zip manually)"
    fi
    ;;

  Linux)
    echo "==> Packaging Linux binary..."
    cp "target/release/$BIN_NAME" "$DIST_DIR/"
    tar czf "$DIST_DIR/VoiceKeys-${VERSION}-linux.tar.gz" -C "$DIST_DIR" "$BIN_NAME"
    echo "==> Done: dist/VoiceKeys-${VERSION}-linux.tar.gz"
    ;;

  *)
    echo "Unknown OS: $OS"
    echo "Binary is at: target/release/$BIN_NAME"
    ;;
esac

echo "==> All release files are in dist/"
ls -lh "$DIST_DIR"
