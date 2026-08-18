dancer-rs — a desktop dancer that anticipates the music
=======================================================

A sprite dances on your desktop, in time with whatever you are playing.

What makes it different from every other desktop dancer: it does not react to
the beat, it *anticipates* it. A move's accent frame — knees deepest, arm fully
raised — is scheduled to land ON the beat, which means the move has to start
before it. Reacting is always late by however long it takes to notice.


Getting started
---------------

1. Unzip anywhere you like and run dancer-rs.exe. Everything it writes stays in
   this folder. There is no installer and nothing goes in the registry.

2. A dancer appears. Drag it with the left mouse button. Its icon appears in the
   system tray — that is where the settings are.

3. Point it at your music and let it analyse:

       dancer-rs.exe --scan D:\music

   This takes a few minutes for a large library and only has to be done once.
   Results are cached in scores.db beside the exe.

   To avoid retyping the folder, put it in config.toml instead and then just run
   `dancer-rs.exe --scan`:

       [library]
       folders = ["D:/music"]

4. Play something in any player — foobar2000, AIMP, VLC, Winamp, Yandex Music,
   a browser tab. Windows tells us what is playing and where, and if the track
   is one we have analysed, the dancer locks to its beat grid.


Getting it exactly in time
--------------------------

There is always a fixed delay between "the player says 42.0 seconds" and the
sound actually reaching your ears — the player's buffer, the DAC, Bluetooth if
you use it. The tray menu has an offset nudge for exactly this.

Watch the dancer against the music and nudge until the accents land. 5 ms steps
for fine work, 25 ms for getting close. It saves as you go.

At 128 BPM one beat is 469 ms, so this is worth doing — leaving it wrong can put
the dancer more than half a beat out.


What does not sync, and why
---------------------------

Tracks you own and have analysed: full sync.

Streamed tracks: the dancer follows play, pause and track changes, but has no
beat grid, so it falls back to a fixed-rate loop. That is a design decision, not
a bug — analysing music needs a file to read, and there is no file.

The one exception is Yandex Music. If you sign in, a track you are playing can
be fetched, analysed, and the audio deleted immediately, keeping only the beat
grid. Nothing is stored and nothing is shared.

Sign in from the tray menu — "Sign in to Yandex Music...". A code appears, a
Yandex page opens in your browser, you type the code in. You authenticate with
Yandex and never with us, and the dancer keeps dancing while you do it.

It is off until you sign in; signing in is the request, so there is nothing else
to switch on. The token is checked each time the app starts, and if it has
expired or you revoked it, you are asked whether to sign in again.

Your token is stored in config.toml in plain text. Treat that file as a
credential, and revoke it at https://id.yandex.ru/security if it ever gets out.


Your own artwork
----------------

Any FAOSDance or Fruity Dance sheet works: a PNG that is 8 cells wide with one
row per animation, plus a .txt naming the rows one per line.

Drop it in assets\ and pick it from the tray, under Dancer. Or set it by hand:

    [sprite]
    sheet = "mysheet.png"

When nothing is playing the dancer loops one row at a resting tempo. Which row
and how fast are both settings:

    [sprite]
    idle_row = "Stepping"    # defaults to default_row

    [playback]
    idle_bpm = 75.0          # 60-90 reads as calm

Worth setting idle_row if your sheet's resting pose barely moves — FL-Chan's
"Waiting" is seven identical frames, so it looks like a still image.

A .toml sidecar next to the PNG unlocks the choreography — which cell is the
accent, how many beats a loop takes, and what kind of movement each row is. See
assets\default.toml for a worked example, and check your work with:

    dancer-rs.exe assets\mysheet.png --check-sheet


Where to get dancers, and who owns them
---------------------------------------

The tray menu links to a couple of places, and so does this:

  FL-Chan      The dancer from FL Studio's Fruity Dance, published by
               Image-Line themselves:
https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/content/FLChan_HD.zip

  Umamusume    Sheets by Steak_Bananite, on GameBanana:
https://gamebanana.com/tools/21924

**These are not ours, and downloading them does not make them yours.**

Every sheet on those pages belongs to whoever made it. FL-Chan is Image-Line's.
The Umamusume sheets were made by Steak_Bananite, with most of the Tanuki sprites
supplied by vonvan, and the characters themselves belong to Cygames, Inc., who
made Umamusume: Pretty Derby. All of it is published on its own terms; read
them.

dancer-rs does not host, bundle or redistribute any of it, and please do not
pass sheets on with copies of this app. The only artwork shipped here is the
plain default sheet, which exists precisely so that nothing else has to be.


When something looks wrong
--------------------------

The app writes dancer-rs.log beside the exe: what it saw playing, which media
session it followed, and every state change. It rotates at 2 MB and keeps one
previous file, so it cannot grow without bound.

If the dancer stops following your music, that file will say whether it lost the
track, could not find a beat grid for it, or is following a different app's media
session. Set RUST_LOG=dancer_rs=debug for more detail.


Controls
--------

  Left drag        move the dancer
  Middle click     toggle anticipation (the A/B — try it on a chorus)
  Right click      quit
  Tray menu        everything else

If you turn on click-through, the sprite stops receiving the mouse entirely, so
the tray becomes the only way to turn it back off.


Full options
------------

  dancer-rs.exe --help


Licence
-------

MIT — see LICENSE. The sprite sheet format is inherited from FAOSDance (MIT).
Beat tracking is beat-this; its model weights carry their own upstream licence.
No artwork here is covered by that licence.
