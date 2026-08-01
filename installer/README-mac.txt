========================================
  Voice Keys - Getting Started (Mac)
========================================

Voice Keys lets you talk instead of typing. Press two keys, say what you
want, press them again, and your words appear in whatever you were typing in.


1. INSTALL

   Drag "Voice Keys.app" into your Applications folder.


2. THE FIRST TIME YOU OPEN IT

   Your Mac will probably say Voice Keys "can't be opened because Apple
   cannot check it for malicious software."

   That's expected. It happens because we haven't paid Apple's yearly fee
   to have the app officially stamped. To get past it:

     Right-click Voice Keys in your Applications folder (or hold Control
     and click it), choose Open, then click Open again.

   You only need to do this once.


3. LET IT USE YOUR MIC AND KEYBOARD

   Your Mac will ask permission. Go to System Settings > Privacy &
   Security and switch Voice Keys on in all three of these:

     Microphone         - so it can hear you
     Input Monitoring   - so it notices your two-key shortcut
     Accessibility      - so it can type your words into other apps

   Then quit Voice Keys and open it again. Your Mac won't apply the
   change to an app that's already running.


4. USING IT

   Click the icon in your menu bar to open settings, and paste in your
   Deepgram key. (Deepgram is the service that turns your speech into
   text. A free account is plenty.)

   Then, any time you want to talk instead of type:

     - Hold the first key and tap the second. It starts listening.
     - Say what you want.
     - Press the same two keys again. Your words get typed out.

   While it's listening, you'll see the time counting up next to the
   menu bar icon.


5. HAVE IT START AUTOMATICALLY (optional)

   System Settings > General > Login Items > click "+" > pick Voice Keys.


6. WHERE YOUR FILES ARE

   Settings and your key:  ~/.config/voicekeys/config.yaml
   Activity log:           ~/.config/voicekeys/voicekeys.log
   Message history:        ~/.config/voicekeys/transcripts.txt

   To see these in Finder, open Terminal and run:

     open ~/.config/voicekeys

   ABOUT YOUR MESSAGE HISTORY: everything you dictate is saved to that
   file and kept forever. It's an ordinary text file with no password on
   it, so anyone who can use your computer account can read everything
   you've ever said. Backups and things like iCloud copy it too.

   You can open it any time by clicking "Message history" at the bottom
   of the app. To wipe it, just delete the file - Voice Keys will start
   a fresh one.


7. REMOVING IT

   Drag Voice Keys from Applications to the Trash, then run:

     rm -rf ~/.config/voicekeys

   That second step deletes your settings and your message history too.
