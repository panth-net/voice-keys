========================================
  Voice Keys - Getting Started (Windows)
========================================

Voice Keys lets you talk instead of typing. Press two keys, say what you
want, press them again, and your words appear in whatever you were typing in.


1. INSTALL

   Double-click "install.bat" in this folder. It puts Voice Keys on your
   computer and adds it to the Start Menu.

   You don't need administrator rights.


2. MAKE THE ICON VISIBLE

   Voice Keys lives in the system tray, at the bottom-right of your
   taskbar next to the clock. Windows hides new icons there by default,
   so you'll want to bring it out:

     Right-click the taskbar > Taskbar settings
     Scroll down to "Other system tray icons"
     Switch "voicekeys.exe" on

   Now you can see at a glance when it's listening.


3. LET IT USE YOUR MICROPHONE

   Go to Settings > Privacy & security > Microphone and turn on both:

     "Microphone access"
     "Let desktop apps access your microphone"

   Then quit Voice Keys and open it again.

   Nothing else is needed - Windows doesn't ask for keyboard permission
   the way a Mac does.


4. USING IT

   Click the tray icon to open settings, and paste in your Deepgram key.
   (Deepgram is the service that turns your speech into text. A free
   account is plenty.)

   Then, any time you want to talk instead of type:

     - Hold the first key and tap the second. It starts listening.
     - Say what you want.
     - Press the same two keys again. Your words get typed out.


5. WHERE YOUR FILES ARE

   The app itself:         %LOCALAPPDATA%\VoiceKeys\voicekeys.exe
   Settings and your key:  %APPDATA%\VoiceKeys\config.yaml
   Activity log:           %APPDATA%\VoiceKeys\voicekeys.log
   Message history:        %APPDATA%\VoiceKeys\transcripts.txt

   To open one of those folders, press the Windows key and R together,
   paste the path in, and press Enter.

   ABOUT YOUR MESSAGE HISTORY: everything you dictate is saved to that
   file and kept forever. It's an ordinary text file with no password on
   it, so anyone who can use your computer account can read everything
   you've ever said. Backups and things like OneDrive copy it too.

   You can open it any time by clicking "Message history" at the bottom
   of the app. To wipe it, just delete the file - Voice Keys will start
   a fresh one.


6. REMOVING IT

   Delete these two folders:

     %LOCALAPPDATA%\VoiceKeys
     %APPDATA%\VoiceKeys

   Then remove the Start Menu shortcut: search for "Voice Keys" in the
   Start Menu, right-click it and choose Delete.

   If you set it to start automatically, also delete:

     %APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\Voice Keys.lnk
