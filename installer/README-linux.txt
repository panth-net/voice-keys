========================================
  Voice Keys - Getting Started (Linux)
========================================

Voice Keys lets you talk instead of typing. Press two keys, say what you
want, press them again, and your words appear in whatever you were typing in.


1. INSTALL WHAT IT NEEDS

   On Ubuntu or Debian:

     sudo apt install libasound2t64 libx11-6 libxtst6 libxdo3 \
       libgtk-3-0t64 libwebkit2gtk-4.1-0 libayatana-appindicator3-1 \
       libnotify-bin

   On older releases, drop the "t64" from the two names that have it.


2. RUN IT

   Make it executable, then start it:

     chmod +x voicekeys
     ./voicekeys

   It lives in your system tray rather than opening a window. Click the tray
   icon to open its settings.

   To put it in your applications menu instead, move it somewhere permanent:

     mkdir -p ~/.local/share/voicekeys
     cp voicekeys icon.png ~/.local/share/voicekeys/
     chmod +x ~/.local/share/voicekeys/voicekeys

   Then create ~/.local/share/applications/voicekeys.desktop containing:

     [Desktop Entry]
     Type=Application
     Name=Voice Keys
     Exec=%h/.local/share/voicekeys/voicekeys
     Icon=%h/.local/share/voicekeys/icon.png
     Terminal=false
     Categories=Utility;AudioVideo;


3. A NOTE ABOUT X11 AND WAYLAND

   Linux has two ways of running the desktop. Voice Keys was built for X11.
   On Wayland it will start and run, but the keyboard shortcuts may not work
   reliably. Recent Ubuntu versions use Wayland by default; you can pick
   "Ubuntu on Xorg" from the gear icon on the login screen.


4. USING IT

   Click the tray icon to open settings, and paste in your Deepgram key.
   (Deepgram is the service that turns your speech into text. A free
   account is plenty.)

   Then, any time you want to talk instead of type:

     - Hold the first key and tap the second. It starts listening.
     - Say what you want.
     - Press the same two keys again. Your words get typed out.


5. HAVE IT START AUTOMATICALLY (optional)

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


6. WHERE YOUR FILES ARE

   Settings and your key:  ~/.config/voicekeys/config.yaml
   Activity log:           ~/.config/voicekeys/voicekeys.log
   Message history:        ~/.config/voicekeys/transcripts.txt

   ABOUT YOUR MESSAGE HISTORY: everything you dictate is saved to that
   file and kept forever. It's an ordinary text file with no password on
   it, so anyone who can use your computer account can read everything
   you've ever said. Backups and cloud-synced folders copy it too.

   You can open it any time by clicking "Message history" at the bottom
   of the app. To wipe it, just delete the file - Voice Keys will start
   a fresh one.


7. REMOVING IT

   rm -rf ~/.local/share/voicekeys ~/.config/voicekeys
   rm -f ~/.local/share/applications/voicekeys.desktop
