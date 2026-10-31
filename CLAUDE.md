# Subtitle Adder

Videoya otomatik altyazı üreten, tamamen yerel çalışan masaüstü uygulaması. Arayüz her platformda native (önce macOS/SwiftUI, sonra Windows/WinUI); iş ortak bir Python motorunda yapılır.

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

- `{"event": "stage", "stage": "extracting_audio|downloading_model|loading_model|transcribing|segmenting", "fraction": 0.42 | null}`
- `{"event": "models", "models": [{"id", "backend", "languages", "size_mb", "disk_mb", "speed", "accuracy", "downloaded"}]}`
- `{"event": "deleted", "id": "small", "freed_mb": 464}`
- `{"event": "result", "language", "duration", "audio", "outputs": [...], "sentences": [{"id", "start", "end", "text", "words": [{"start", "end", "text"}]}]}`
- `{"event": "error", "message": "..."}` (çıkış kodu 1)

`models` olayında `backend` `"whisper"` (faster-whisper) ya da `"onnx"` (onnx-asr); `languages` Whisper'da `null` (tüm diller), diğerlerinde desteklenen ISO 639-1 kodlarının listesidir. Katalog kimlikleri: `base`, `small`, `medium`, `large-v3-turbo`, `large-v3` (Whisper) ve `parakeet-v3` (NVIDIA Parakeet TDT 0.6B v3), `canary-1b-v2` (NVIDIA Canary 1B v2), `gigaam-v3` (GigaAM v3 e2e RNN-T, yalnız `ru`). Parakeet ve Canary 25 Avrupa dilini (`pt`, `es`, `ru` dahil) destekler; `tr` yalnız Whisper'dadır. Seçilen dil modelin `languages` listesinde yoksa motor `error` olayıyla Türkçe açıklama döndürür (ör. "GigaAM yalnız Rusça destekler.").

`downloading_model` yalnız katalogdaki (`models.py`) bir model önbellekte yoksa, `loading_model`'den önce yayınlanır; `fraction` indirilen baytın toplam bayta oranıdır (0..1, en fazla ~10 Hz). onnx modellerinde ortak silero VAD dosyası da bu aşamada indirilir. Katalog dışı adlar (`turbo`, `tiny` vb.) faster-whisper'ın kendi indirmesine bırakılır, bu aşama yayınlanmaz.

`subtitle-engine render --format srt [--out F]`: stdin'den cümle JSON dizisi alır (uygulamada düzenlenmiş hali), altyazı metni yazar. Altyazı biçimleri yalnız motorda üretilir.

`subtitle-engine models [--json]`: model kataloğunu (`base`, `gigaam-v3`, `small`, `parakeet-v3`, `canary-1b-v2`, `medium`, `large-v3-turbo`, `large-v3`) yaklaşık boyut, göreli hız/doğruluk, backend, desteklenen diller ve yerel önbellek durumuyla listeler; ağa çıkmaz. `disk_mb` modelin önbellekteki repo'sunun gerçek disk kullanımıdır (MB, tam sayıya yuvarlanmış; ortak VAD hariç); indirilmemişse `null`. `--json` tek satır `models` olayı yazar, aksi halde okunur tablo.

`subtitle-engine delete-model <id> [--json]`: indirilmiş modelin önbellekteki repo'sunu tüm revizyonları ve blob'larıyla siler; yalnız o repo klasörüne dokunur, ortak silero VAD ve diğer modeller korunur. `--json` tek satır `{"event": "deleted", "id": "...", "freed_mb": 123}` yazar. Katalog dışı ya da indirilmemiş modelde anlaşılır Türkçe `error` olayı ve çıkış kodu 1 döner.

stderr serbest log'dur (model yükleme uyarıları vb.); uygulamalar onu dosyaya yönlendirir, protokol değildir. Protokol değişirse `cli.py`'deki `PROTOCOL_VERSION`, `EngineEvent.swift` ve bu bölüm birlikte güncellenir. Sürüm 1'de kalır: `downloading_model` aşaması, `models` komutu, `models` olayındaki `backend`/`languages`/`disk_mb` alanları, `delete-model` komutu ve `deleted` olayı ile onnx model kimlikleri geriye uyumlu eklemelerdir.

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

Aynı ajanlar DeepSeek ile de çalışır (pi, opencode sağlayıcısı, thinking max):

```bash
scripts/pi-agent.sh subtitle-engineer "<görev>"
PI_MODEL=opencode-go/deepseek-v4-pro:max scripts/pi-agent.sh stt-engineer "<görev>"
```

Betik `.claude/agents/<ajan>.md` tanımını sistem promptu olarak verir; explorer ve module-reviewer salt okunurdur. pi oturumları `~/.pi/agent/sessions/` altında kalır.

Ana oturum koordinatördür: işi ajana verir, testlerden ve review'dan geçirip küçük topikal commit alır, istenmeden push yapmaz.

## Kurallar

- Arayüz native ve özenli; iş mantığı arayüze girmez, motordadır.
- Motor platforma özel varsayım yapmaz (sistem ffmpeg'i yok, sabit yol yok); macOS ve Windows'ta aynı çalışır.
- Python ≥3.11 (uv), Swift 6 strict concurrency, macOS 15+.
- UI metinleri ve kullanıcıya giden hata mesajları Türkçe.
- Yalnız yerel: bulut STT ya da sesi dışarı gönderen hiçbir servis yok. Ağ yalnız model indirmek için kullanılır.
- Durum ve devam noktası: `.claude/progress/STATE.md`.
