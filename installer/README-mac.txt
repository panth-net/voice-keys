========================================
  Voice Keys - Setup Guide (macOS)
========================================

1. INSTALL
   Drag "Voice Keys.app" into your Applications folder.

2. FIRST LAUNCH
   This build is ad-hoc signed and not notarised, so macOS will say the
   developer cannot be verified. To get past it:

   - Right-click Voice Keys in Applications > Open > Open

   Or, from Terminal:

     xattr -dr com.apple.quarantine "/Applications/Voice Keys.app"

   You only need to do this once.

3. PERMISSIONS
   macOS will ask you to grant permissions. Allow all three:

   - System Settings > Privacy & Security > Accessibility
   - System Settings > Privacy & Security > Input Monitoring
   - System Settings > Privacy & Security > Microphone

   After granting permissions, quit and reopen Voice Keys.

4. USE IT
   - Click the menu bar icon to open settings and enter your Deepgram API key.
   - Hold the first hotkey, press the second: recording starts.
   - Press the same two keys again to stop. Your speech is transcribed and
     pasted into whatever app you were typing in.
   - Recording time is shown next to the menu bar icon.

5. START ON LOGIN (optional)
   System Settings > General > Login Items > click "+" > select Voice Keys.

6. FILES & CONFIG
   Config & API key:  ~/.config/voicekeys/config.yaml
   Log file:          ~/.config/voicekeys/voicekeys.log

   To open in Finder: open ~/.config/voicekeys

7. UNINSTALL
   - Drag Voice Keys from Applications to the Trash.
   - Delete config: rm -rf ~/.config/voicekeys
