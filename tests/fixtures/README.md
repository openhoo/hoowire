# Synthetic SDK fixtures

These tiny fixtures were generated for HooWire compatibility tests. They do not
contain Teamwire software, logos, user data or licensed media.

- `pixel.jpg`: one gray pixel, a hand-assembled baseline JPEG with one DC and
  one AC Huffman code.
- `pixel.png`: a one-pixel PNG used for the bot avatar endpoint, which requires
  PNG input while chat avatar input requires JPEG.
- `tone.wav`: 100 ms of a 440 Hz sine wave, mono 8 kHz signed 16-bit PCM.
- `clip.mp4`: one 16 by 16 green H.264 baseline frame generated with FFmpeg:

  ```sh
  ffmpeg -f lavfi -i color=c=green:s=16x16:r=10:d=0.1 \
    -c:v libx264 -profile:v baseline -level 1.0 -pix_fmt yuv420p -an \
    -movflags +faststart -metadata comment='HooWire synthetic conformance fixture' clip.mp4
  ```

All fixtures are covered by the repository's license.
