# Original MiniMax contract fixtures

These are procedural, public-safe media authored for NodeCraft, with no vendor media or API call. FFmpeg/ffprobe are local developer tools and are not bundled by this change.

MP3 recipe (32 kHz, mono, 128 kbit/s, one second):

    ffmpeg -f lavfi -i sine=frequency=330:sample_rate=32000:duration=1 -ac 1 -c:a libmp3lame -b:a 128k -write_xing 0 -id3v2_version 3 minimax27-tone.mp3

MP4 recipe (five seconds of moving test pattern plus an original tone):

    ffmpeg -f lavfi -i testsrc2=size=64x48:rate=24:duration=5 -f lavfi -i sine=frequency=440:sample_rate=44100:duration=5 -c:v libx264 -pix_fmt yuv420p -crf 32 -c:a aac -b:a 32k -shortest -movflags +faststart minimax27-motion.mp4

WAV fixtures are authored in Rust: ordinary RIFF PCM, 44.1 kHz, 16-bit stereo silence. The 120-second fixture exceeds 16 MiB and exercises native worker retention and save/reopen. None demonstrates provider artistic quality.
