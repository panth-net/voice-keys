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

OS="$(uname -s)"

echo "==> Building release binary..."
# Rust bakes the source path of every panic location into the binary, which
# otherwise leaks the building machine's home directory (and username) into
# every artifact we ship. Rewrite those prefixes so published builds carry no
# trace of whoever compiled them.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME/.cargo=/cargo --remap-path-prefix=$SCRIPT_DIR=/voicekeys --remap-path-prefix=$HOME=/home"

if [ "$OS" = "Darwin" ]; then
  # Build for both Mac architectures and fuse them. Building only for whichever
  # machine happens to run this would quietly ship an app that won't launch for
  # everyone on the other one — and CI runners are Apple Silicon.
  echo "==> Building universal binary (arm64 + x86_64)..."
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
  cargo build --release --target aarch64-apple-darwin
  cargo build --release --target x86_64-apple-darwin
  RELEASE_BIN="target/universal-apple-darwin/release/$BIN_NAME"
  mkdir -p "$(dirname "$RELEASE_BIN")"
  lipo -create -output "$RELEASE_BIN" \
    "target/aarch64-apple-darwin/release/$BIN_NAME" \
    "target/x86_64-apple-darwin/release/$BIN_NAME"
  echo "==> Universal binary: $(lipo -archs "$RELEASE_BIN")"
else
  cargo build --release
  RELEASE_BIN="target/release/$BIN_NAME"
fi

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

case "$OS" in
  Darwin)
    echo "==> Packaging macOS .app bundle..."
    APP_DIR="$DIST_DIR/$APP_NAME.app"
    mkdir -p "$APP_DIR/Contents/MacOS"
    mkdir -p "$APP_DIR/Contents/Resources"
    cp "$RELEASE_BIN" "$APP_DIR/Contents/MacOS/"
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
    # clear the quarantine flag on first launch. See "On a Mac, the first time
    # you open it" in the README.
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

    # The unsigned build trips Gatekeeper on first launch, and the three
    # permissions it needs aren't guessable. Ship the instructions in the DMG
    # rather than assuming people find their way back to the README.
    cp "installer/README-mac.txt" "$MOUNT_POINT/Read me first.txt"

    # Create symlink to Applications folder
    ln -s /Applications "$MOUNT_POINT/Applications"

    # Set icon positions and background via .DS_Store
    # Use AppleScript to arrange icons nicely.
    #
    # Driving Finder needs a logged-in GUI session, which build machines without
    # a desktop (CI runners) don't have. The layout is pure cosmetics — the DMG
    # mounts and installs fine without it — so never let this sink the build.
    if ! osascript <<EOF
tell application "Finder"
    tell disk "$APP_NAME"
        open
        set current view of container window to icon view
        set toolbar visible of container window to false
        set statusbar visible of container window to false
        set bounds of container window to {400, 200, 900, 500}
        set theViewOptions to the icon view options of container window
        set arrangement of theViewOptions to not arranged
        set icon size of theViewOptions to 80
        set position of item "$APP_NAME.app" of container window to {120, 120}
        set position of item "Applications" of container window to {380, 120}
        set position of item "Read me first.txt" of container window to {250, 240}
        close
    end tell
end tell
EOF
    then
      echo "==> Note: couldn't arrange the DMG window (no Finder session). Continuing."
    fi

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
    cp "${RELEASE_BIN}.exe" "$STAGING/"
    cp "installer/install.bat" "$STAGING/"
    cp "assets/icon.ico" "$STAGING/" 2>/dev/null || true
    cp "installer/README.txt" "$STAGING/Read me first.txt"

    # Windows PowerShell 5.1's Compress-Archive writes backslash path
    # separators, which the ZIP spec forbids. Explorer copes, but unzip, 7-Zip
    # and macOS see one file literally named "VoiceKeys\voicekeys.exe" instead
    # of a folder. 7-Zip writes conformant archives, so reach for it first.
    #
    # Zipping from inside dist/ keeps the paths relative, which also sidesteps
    # having to hand Windows tools an MSYS path.
    ZIP_NAME="VoiceKeys-${VERSION}-windows.zip"
    (
      cd "$DIST_DIR"
      if command -v 7z &>/dev/null; then
        7z a -tzip -bso0 "$ZIP_NAME" VoiceKeys
      elif command -v 7z.exe &>/dev/null; then
        7z.exe a -tzip -bso0 "$ZIP_NAME" VoiceKeys
      elif command -v pwsh &>/dev/null; then
        pwsh -NoProfile -Command "Compress-Archive -Path 'VoiceKeys' -DestinationPath '$ZIP_NAME' -Force"
      elif command -v powershell.exe &>/dev/null; then
        powershell.exe -NoProfile -Command "Compress-Archive -Path 'VoiceKeys' -DestinationPath '$ZIP_NAME' -Force"
      else
        echo "No zip tool found. dist/VoiceKeys/ is staged — zip it by hand." >&2
        exit 1
      fi
    )
    rm -rf "$STAGING"
    echo "==> Done: dist/$ZIP_NAME"
    ;;

  Linux)
    echo "==> Packaging Linux binary..."
    # Stage into a folder so the tarball unpacks tidily and carries its
    # instructions, rather than dropping a bare binary into the user's cwd.
    STAGING="$DIST_DIR/VoiceKeys"
    mkdir -p "$STAGING"
    cp "$RELEASE_BIN" "$STAGING/"
    cp "installer/README-linux.txt" "$STAGING/Read me first.txt"
    cp "assets/icon.png" "$STAGING/"
    tar czf "$DIST_DIR/VoiceKeys-${VERSION}-linux.tar.gz" -C "$DIST_DIR" "VoiceKeys"
    rm -rf "$STAGING"
    echo "==> Done: dist/VoiceKeys-${VERSION}-linux.tar.gz"
    ;;

  *)
    echo "Unknown OS: $OS"
    echo "Binary is at: target/release/$BIN_NAME"
    ;;
esac

echo "==> All release files are in dist/"
ls -lh "$DIST_DIR"
