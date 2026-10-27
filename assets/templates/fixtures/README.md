# Original template fixtures

`blue-tone.mp4` is original NodeCraft material: a 32×32 blue field, a fixed white orientation
mark and an orange marker moving right over twelve frames, with a 440 Hz tone. It is one second
at 12 fps. The motion makes reverse, speed, frame extraction and joins observable. The Stitch
starter plays it forwards and then plays its locally reversed result; it bundles no second copy.

The fixture was authored with this pixel expression, encoded with FFmpeg, and stripped of metadata:

```sh
ffmpeg -hide_banner -loglevel error -y -f lavfi \
  -i "nullsrc=size=32x32:rate=12:duration=1,geq=r='if(between(X,2+2*N,8+2*N)*between(Y,13,20),240,if(between(X,25,28)*between(Y,2,6),240,32))':g='if(between(X,2+2*N,8+2*N)*between(Y,13,20),160,if(between(X,25,28)*between(Y,2,6),240,80))':b='if(between(X,2+2*N,8+2*N)*between(Y,13,20),48,if(between(X,25,28)*between(Y,2,6),240,180))'" \
  -f lavfi -i 'sine=frequency=440:sample_rate=44100:duration=1' \
  -c:v libx264 -crf 18 -pix_fmt yuv420p -c:a aac -shortest \
  -map_metadata -1 -fflags +bitexact -flags:v +bitexact -flags:a +bitexact blue-tone.mp4
```

PNG examples are authored by `Graph::image` in the Rust template author: opaque green with an
asymmetric colored L and a separate square. Their decoded pixels, transformations and keyed alpha
are tested. PDF content is authored there too. The GLB triangle and WAV reuse NodeCraft's original
test fixtures. These assets use the repository's license; no external media or workflow is copied.
