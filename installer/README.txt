========================================
  Voice Keys - Setup Guide (Windows)
========================================

1. INSTALL
   Double-click "install.bat" in this folder.
   It will install Voice Keys and create a Start Menu shortcut.
   No admin rights needed.

2. SHOW THE TRAY ICON
   Voice Keys lives in your system tray (bottom-right of the taskbar).
   Windows hides new tray icons by default. To keep it visible:

   - Right-click the taskbar > Taskbar settings
   - Scroll to "Other system tray icons"
   - Toggle "voicekeys.exe" ON

   This lets you see recording status (elapsed time) at a glance.

3. USE IT
   - Click the tray icon to open settings and enter your Deepgram API key.
   - Hold the first hotkey, press the second: recording starts.
   - Press the same two keys again to stop. Your speech is transcribed and
     pasted into whatever app you were typing in.

4. FILES & CONFIG
   App install location:  %LOCALAPPDATA%\VoiceKeys\voicekeys.exe
   Config & API key:      %APPDATA%\VoiceKeys\config.yaml
   Log file:              %APPDATA%\VoiceKeys\voicekeys.log

   To open either folder, press Win+R and paste the path above.

5. UNINSTALL
   Delete both folders:
   - %LOCALAPPDATA%\VoiceKeys
   - %APPDATA%\VoiceKeys
   Remove the Start Menu shortcut: search "Voice Keys" in Start, right-click > Delete.
   If you added it to startup, also remove: %APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\Voice Keys.lnk
