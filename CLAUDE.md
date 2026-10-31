# Subtitle Adder

Videoya otomatik altyazı üreten masaüstü uygulaması. Arayüz her platformda native (önce macOS/SwiftUI, sonra Windows/WinUI); iş ortak bir Python motorunda yapılır.

## Mimari

```
apps/macos  (SwiftUI)  ─┐
apps/windows (WinUI)   ─┼─ alt süreç + JSON satır olayları ─▶  engine/ (Python)
                        ┘
```

Motor hattı (`engine/src/subtitle_engine/`):

1. **Ses ayırma** — `audio.py` (PyAV): video → 16 kHz mono WAV
2. **Speech-to-text** — `transcribe.py`: `Transcriber` protokolü, varsayılan `FasterWhisperTranscriber` (on-device). Kelime zaman damgalı `Transcript`.
3. **Cümle çıkarma** — `sentences.py`: noktalama/sessizlikle cümle; uzun cümleyi virgülde ya da `max_chars`/`max_duration` sınırında böler.
4. **Altyazı** — `subtitles.py`: SRT / VTT / JSON.

`pipeline.py` dördünü uçtan uca çalıştırır, `cli.py` dışarı açar.

## Motor protokolü (sürüm 1)

`subtitle-engine transcribe <video> --out-dir D [--language tr] [--model small] [--format srt vtt json] --json`
stdout'a satır başına bir JSON olay yazar:

- `{"event": "stage", "stage": "extracting_audio|loading_model|transcribing|segmenting", "fraction": 0.42 | null}`
- `{"event": "result", "language", "duration", "audio", "outputs": [...], "sentences": [{"id", "start", "end", "text", "words": [{"start", "end", "text"}]}]}`
- `{"event": "error", "message": "..."}` (çıkış kodu 1)

`subtitle-engine render --format srt [--out F]`: stdin'den cümle JSON dizisi alır (uygulamada düzenlenmiş hali), altyazı metni yazar. Altyazı biçimleri yalnız motorda üretilir.

stderr serbest log'dur (model indirme ilerlemesi vb.); uygulamalar onu dosyaya yönlendirir, protokol değildir. Protokol değişirse `cli.py`'deki `PROTOCOL_VERSION`, `EngineEvent.swift` ve bu bölüm birlikte güncellenir.

Uygulama motoru şu sırayla bulur: `SUBTITLE_ENGINE` ortam değişkeni → bundle içindeki `subtitle-engine` → geliştirmede `uv run --project engine subtitle-engine`.

## Komutlar

```bash
cd engine && uv sync && uv run pytest              # motor testleri
cd engine && uv run pytest -m slow                 # gerçek STT (samples/demo.mp4)
cd engine && uv run subtitle-engine transcribe ../samples/demo.mp4 -m base -l tr -o ../output -f srt vtt json
cd apps/macos && swift build && swift test         # uygulama + EngineKit testleri
cd apps/macos && ENGINE_E2E=1 swift test           # Swift → motor uçtan uca
cd apps/macos && swift run SubtitleAdderApp
```

Örnek video:

```bash
mkdir -p samples && say -v Yelda -o samples/speech.aiff "Merhaba, bu bir altyazı testidir. Sen ne yapacaksın?"
ffmpeg -y -f lavfi -i color=c=black:s=320x240 -i samples/speech.aiff -shortest -c:v libx264 -c:a aac samples/demo.mp4
```

Whisper modelleri ilk kullanımda Hugging Face önbelleğine iner (`~/.cache/huggingface/`).

## Ajanlar (`.claude/agents/`)

Model dağılımı: keşif Haiku, kod yazan ajanlar Sonnet, review Opus.

- `explorer` (Haiku) — salt okunur keşif, API doğrulama
- `media-engineer` (Sonnet) — ses ayırma, medya (motor)
- `stt-engineer` (Sonnet) — STT backend'leri, modeller (motor)
- `subtitle-engineer` (Sonnet) — cümle bölme, altyazı formatları (motor)
- `macos-ui-engineer` (Sonnet) — SwiftUI uygulaması ve EngineKit
- `module-reviewer` (Opus) — commit öncesi review (kod yazmaz)

Ana oturum koordinatördür: işi ajana verir, testlerden ve review'dan geçirip küçük topikal commit alır, istenmeden push yapmaz.

## Kurallar

- Arayüz native ve özenli; iş mantığı arayüze girmez, motordadır.
- Motor platforma özel varsayım yapmaz (sistem ffmpeg'i yok, sabit yol yok); macOS ve Windows'ta aynı çalışır.
- Python ≥3.11 (uv), Swift 6 strict concurrency, macOS 15+.
- UI metinleri ve kullanıcıya giden hata mesajları Türkçe.
- Varsayılan on-device; bulut servisi opsiyonel olur, anahtar koda girmez.
- Durum ve devam noktası: `.claude/progress/STATE.md`.
