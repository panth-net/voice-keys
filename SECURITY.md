# Security

## Found a problem? Tell us privately

If you've found a way Voice Keys could be misused or could put someone at risk,
please don't post it publicly. Write to us through
<https://www.pantheonnetwork.co/contact> and put "Voice Keys security" in the
subject line.

It helps if you tell us:

- which version of Voice Keys you're using
- which computer and operating system
- what you did, step by step
- what you think someone could do with it

We aim to have it fixed asap and will respond to you and let you know when we've done so. We're happy to thank you by
name when we announce the fix, unless you'd rather we didn't. We don't pay for
bug reports.


## What Voice Keys does with your voice and your words

### Your recordings

When you finish speaking, Voice Keys sends that recording to Deepgram, the
company that turns speech into text. That's the only place it goes. Deepgram
decides how long they keep it — that's covered by their [privacy policy](https://deepgram.com/data-security), not
ours.

Voice Keys doesn't send anything anywhere else. No tracking, no usage
statistics, no automatic update checks.

### Your words are saved on your computer, forever

Every time Voice Keys writes out what you said, it also adds it to a file
called `transcripts.txt`. You can open it from the app — there's a **Message
history** link at the bottom.

Here's what that means in practice, and it's worth understanding:

- **It's an ordinary text file with no password on it.** Anyone who can sit
  down at your computer and use your account can open it and read everything
  you've ever dictated.
- **Nothing is ever removed.** Voice Keys only ever adds to it. It doesn't
  trim old entries or start a new file.
- **Backups copy it too.** If you back up your computer, or that folder is in
  iCloud, Dropbox, OneDrive or similar, your dictation history is copied there
  as well.

None of that is accidental — a history you can look back on is the point. But
if you dictate something private, that's where it ends up.

**To wipe it, delete the file.** FYI Voice Keys will quietly start a new one when you voice note next:

```bash
rm ~/.config/voicekeys/transcripts.txt
```

On Windows it's at `%APPDATA%\VoiceKeys\transcripts.txt`.

### Voice Keys is not watching what you type

To notice your keyboard shortcut, Voice Keys has to be able to see key presses.
There's no way around that — it's how every app with a global shortcut works.

What it does with them matters, though. Each key press is checked against your
chosen shortcut and then thrown away. Nothing you type is stored or sent
anywhere.

On a Mac, Voice Keys asks the system for a *listen-only* connection to the
keyboard. That's a real technical limit, not just a promise: it means the app
is not able to change, block, or fake your key presses even if it tried.

There is a diagnostic mode that does record key presses, for when a shortcut
isn't working. It's off unless you deliberately switch it on, and the README
explains it under "Checking the keyboard shortcut".

## Things worth knowing

- **Your Deepgram key is stored as ordinary text.** It sits in `config.yaml`
  alongside your other settings. It isn't scrambled, and it isn't kept in your
  computer's password manager. Anyone who can read that file can use your
  Deepgram account. If that worries you, create a key that's limited to just
  this purpose rather than using one that can do everything.

- **On a Mac, Voice Keys asks for some powerful permissions.** It needs
  Accessibility and Input Monitoring, and those are broad — you're right to
  think twice about granting them to anything. The listen-only limit described
  above is what keeps this from being as risky as it sounds.

- **Downloaded copies aren't stamped by Apple.** We haven't paid for Apple's
  developer programme, so your Mac will warn you the first time you open it.
  The README explains how to get past that. It also means your Mac can't
  independently confirm the file wasn't tampered with after we built it — if
  you'd rather not take our word for it, build it yourself from the source.

- **The log file can contain small details about you.** `voicekeys.log`
  records things like the name of your microphone and any error messages from
  Deepgram. It does *not* contain what you said. Still, have a quick look
  before you send it to anyone.

- **Two optional modes save more than the defaults do.** Starting Voice Keys
  with `RUST_LOG=debug` saves the beginning of each thing you say into the log
  file. Starting it with `VOICEKEYS_DEBUG_KEYS=2` records the first 200 keys
  you press, whether or not they're your shortcut. Both are off unless you turn
  them on, and both are worth deleting afterwards.
