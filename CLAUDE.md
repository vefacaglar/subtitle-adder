# Subtitle Adder

Videoya otomatik altyazı üreten native masaüstü uygulaması. Önce macOS (Swift), sonra Windows (ayrı native app).

## Hat

1. **Ses ayırma** — `AudioExtractor` (AVFoundation): video → 16 kHz mono WAV
2. **Speech-to-text** — `Transcriber` protokolü; varsayılan `WhisperKitTranscriber` (on-device, CoreML). Kelime zaman damgalı `Transcript` döner.
3. **Cümle çıkarma** — `SentenceSegmenter`: noktalama/sessizlikle cümle, uzun cümleyi virgülde ya da `maxChars`/`maxDuration` sınırında böler.
4. **Altyazı** — `SubtitleWriter`: SRT / VTT / JSON.

`SubtitlePipeline` bu dört adımı uçtan uca çalıştırır.

## Yapı

- `Sources/SubtitleCore/` — platformdan bağımsız çekirdek (Windows'a taşınacak kurallar burada, testlerle belgeli)
- `Sources/WhisperTranscriber/` — WhisperKit backend'i
- `Sources/subtitle-cli/` — test/otomasyon için CLI
- `Sources/SubtitleAdderApp/` — SwiftUI macOS uygulaması
- `Tests/SubtitleCoreTests/` — Swift Testing

## Komutlar

```bash
swift build
swift test
swift run subtitle-cli samples/demo.mp4 -m base -l tr -f srt,vtt,json   # çıktı: output/
swift run SubtitleAdderApp
```

Örnek video üretmek için:

```bash
mkdir -p samples && say -v Yelda -o samples/speech.aiff "Merhaba, bu bir altyazı testidir. Sen ne yapacaksın?"
ffmpeg -y -f lavfi -i color=c=black:s=320x240 -i samples/speech.aiff -shortest -c:v libx264 -c:a aac samples/demo.mp4
```

WhisperKit modelleri ilk kullanımda indirilir (`~/Documents/huggingface/`).

## Ajanlar (`.claude/agents/`)

- `media-engineer` — ses ayırma, medya
- `stt-engineer` — STT backend'leri, modeller
- `subtitle-engineer` — cümle bölme, altyazı formatları
- `macos-ui-engineer` — SwiftUI uygulaması
- `module-reviewer` — commit öncesi review (kod yazmaz)

Ana oturum koordinatördür: işi ajana verir, `swift test` ve review'dan geçirip küçük topikal commit alır, push istenmeden yapılmaz.

## Kurallar

- Swift 6 strict concurrency, macOS 15+.
- UI metinleri ve hata mesajları Türkçe.
- Varsayılan on-device; bulut servisi eklenecekse opsiyonel olur, anahtar koda girmez.
- Durum ve devam noktası: `.claude/progress/STATE.md`.
