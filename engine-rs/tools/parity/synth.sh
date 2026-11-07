# Builds the parity samples from the raw SAPI WAVs written by synth.ps1 (run synth.ps1 -Out <PARITY_DIR>/samples first).
# Turkish has a single SAPI voice (Tolga); turns 2, 4, 6 are pitch-shifted up to act as a second speaker.
set -e
ff="${FFMPEG_DIR:-$LOCALAPPDATA/devtools/ffmpeg-n8.1-latest-win64-gpl-shared-8.1}/bin/ffmpeg.exe"
cd "${PARITY_DIR:-$(cygpath -u "$TEMP")/se-parity}/samples"
for i in 1 3 5; do cp tr-$i-raw.wav tr-$i.wav; done
for i in 2 4 6; do "$ff" -hide_banner -loglevel error -y -i tr-$i-raw.wav -af 'asetrate=16000*1.35,aresample=16000,atempo=0.8' -ac 1 -ar 16000 tr-$i.wav; done
for n in en tr; do
  "$ff" -hide_banner -loglevel error -y $(for i in 1 2 3 4 5 6; do echo -n "-i $n-$i.wav "; done) -filter_complex "concat=n=6:v=0:a=1[a]" -map "[a]" $n.wav
  "$ff" -hide_banner -loglevel error -y -f lavfi -i color=c=black:s=320x240:r=10 -i $n.wav -shortest -c:v libx264 -pix_fmt yuv420p -c:a aac dialog-$n.mp4
done
"$ff" -hide_banner -loglevel error -y -i dialog-en.mp4 -t 6 -c:v libvpx-vp9 -b:v 200k -c:a libopus clip.mkv
rm -f tr-*.wav en-*.wav
ls -la
