A desktop dancer that **anticipates** the music: a move's accent frame is scheduled
to land on the beat, so the move has to start before it. Reacting is always late by
however long it takes to notice.

**Windows only.** Unzip anywhere and run `dancer-rs.exe` — everything it writes stays
in that folder, there is no installer and nothing goes in the registry. Point it at
your music with `--scan`, then play something in any player: Windows says what is
playing and where, and the dancer locks to that track's beat grid. README.txt in the
zip covers the rest, including the tray offset nudge that gets the accents actually
landing.

Built for `x86_64-pc-windows-gnu`. The zip carries the executable, the plain default
sheet, and the beat-tracking ONNX weights — pinned by SHA-256 at packaging time, so
a release cannot quietly ship weights it was not tested against.

**No sprite sheets are distributed here beyond the plain default one.** FL-Chan is
Image-Line's artwork and is deliberately absent. The app links to places sheets can
be downloaded from and warns before opening each one; those links are pointers, not
permission, and the artwork belongs to whoever made it.

Verify the download against `PACKAGE.zip.sha256`.
