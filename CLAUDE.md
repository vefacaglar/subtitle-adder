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
5. **Konuşmacı ayrıştırma (isteğe bağlı)** — `diarize.py`: sherpa-onnx ile `[(start, end, speaker)]`; kelimelere konuşmacı atar, `sentences.py` konuşmacı değişiminde böler.

`pipeline.py` beş adımı (diarization isteğe bağlı) uçtan uca çalıştırır, `cli.py` dışarı açar.

## Motor protokolü (sürüm 1)

`subtitle-engine transcribe <video> --out-dir D [--language tr] [--model small] [--format srt vtt json] --json`
stdout'a satır başına bir JSON olay yazar:

- `{"event": "stage", "stage": "extracting_audio|downloading_model|loading_model|transcribing|diarizing|segmenting|translating", "fraction": 0.42 | null}`
- `{"event": "models", "models": [{"id", "kind", "backend", "languages", "size_mb", "disk_mb", "speed", "accuracy", "license", "downloaded"}]}`
- `{"event": "deleted", "id": "small", "freed_mb": 464}`
- `{"event": "result", "language", "duration", "audio", "outputs": [...], "sentences": [{"id", "start", "end", "text", "speaker": 0 | null, "words": [{"start", "end", "text"}]}]}`
- `{"event": "translation", "language": "es", "sentences": [{"id", "start", "end", "text", "speaker", "words": []}]}`
- `{"event": "error", "message": "..."}` (çıkış kodu 1)

`models` olayında `kind` `"transcription"` (konuşma tanıma), `"translation"` (çeviri) ya da `"diarization"` (konuşmacı ayrıştırma); `backend` `"whisper"` (faster-whisper), `"onnx"` (onnx-asr), `"ctranslate2"` (çeviri) ya da `"sherpa"` (sherpa-onnx, diarization). `license` model kartındaki SPDX kısa adıdır; `speaker-diarization` iki modelden oluştuğu için SPDX ifadesidir (`"MIT AND Apache-2.0"`). `languages` Whisper'da ve `speaker-diarization`'da `null` (tüm diller), diğerlerinde desteklenen ISO 639-1 kodlarının listesidir. Katalog kimlikleri: `base`, `small`, `medium`, `large-v3-turbo`, `large-v3` (Whisper, MIT) ve `parakeet-v3` (NVIDIA Parakeet TDT 0.6B v3, CC-BY-4.0), `canary-1b-v2` (NVIDIA Canary 1B v2, CC-BY-4.0), `gigaam-v3` (GigaAM v3 e2e RNN-T, yalnız `ru`, MIT); konuşmacı ayrıştırma modeli `speaker-diarization` (pyannote segmentation-3.0 int8, MIT + 3D-Speaker CAM++ embedding, Apache-2.0; `license` `"MIT AND Apache-2.0"`, ~29 MB, iki HF deposundan tek girdi); çeviri modelleri `madlad-3b` (MADLAD-400 3B, Apache-2.0, varsayılan) ve `nllb-1.3b` (NLLB-200 1.3B, CC-BY-NC-4.0, ticari kullanım yok). Parakeet ve Canary 25 Avrupa dilini (`pt`, `es`, `ru` dahil) destekler; `tr` yalnız Whisper'dadır. Çeviri modelleri `tr`, `en`, `pt`, `es`, `ru` dillerini destekler. Seçilen dil modelin `languages` listesinde yoksa motor `error` olayıyla Türkçe açıklama döndürür (ör. "GigaAM yalnız Rusça destekler.").

`downloading_model` yalnız katalogdaki (`models.py`) bir model önbellekte yoksa, `loading_model`'den önce yayınlanır; `fraction` indirilen baytın toplam bayta oranıdır (0..1, en fazla ~10 Hz). onnx modellerinde ortak silero VAD dosyası da bu aşamada indirilir. Katalog dışı adlar (`turbo`, `tiny` vb.) faster-whisper'ın kendi indirmesine bırakılır, bu aşama yayınlanmaz.

With `--diarize`, `downloading_model` can appear twice in one run: first for the transcription model (if missing), then for `speaker-diarization` (if missing), both before `loading_model`; each run goes 0..1 on its own. `transcribe -m` accepts only `kind` `"transcription"` catalog models: anything else (for example `speaker-diarization`, `madlad-3b`) gets a Turkish `error` before any download. `translate --model` likewise accepts only translation models.

`subtitle-engine transcribe ... --diarize [--speakers N]`: speaker diarization (fully local, off by default). When on, `downloading_model` (only if the `speaker-diarization` model is not cached; emitted before `loading_model`) and a `diarizing` stage (`fraction` 0..1, after `transcribing`) are emitted, and every `result` sentence carries `speaker`: a 0-based integer numbered by first appearance (0 is the first person to speak), or `null` when diarization was off. The speaker count is automatic (clustering threshold, speakers with under 1 s of speech are dropped as noise) unless `--speakers N` is given; `--speakers` without `--diarize` is an error. Auto mode also merges clusters whose centroid embeddings (bounded sample, same CAM++ model) have cosine similarity of at least 0.65, because clustering over-splits long recordings; this is skipped with `--speakers N`. Each word gets the speaker with the largest time overlap (nearest segment if none); a lone word between two longer runs of one speaker joins them unless it is a complete short reply, and a speaker change in the middle of a sentence snaps to the nearest sentence end when that moves at most 2 words / 0.8 s. `sentences.py` splits at every speaker change, so a subtitle never contains two speakers. `translate` keeps `speaker`.

Speaker dash rule (SRT and VTT, computed at render time, never stored in `text`): if the sentences contain at least two distinct non-null speakers, every sentence whose speaker differs from the previous known speaker (sentences with a `null` speaker are skipped and do not reset it; the first sentence included) is prefixed with `- ` unless its text already starts with `- ` or `– `; a sentence continuing with the same speaker gets none. With one speaker or all `null` there are no dashes. JSON keeps `speaker` as is.

`subtitle-engine render --format srt [--out F]`: stdin'den cümle JSON dizisi alır (uygulamada düzenlenmiş hali; varsa `speaker` alanı okunur, çizgi kuralı burada uygulanır), altyazı metni yazar. Altyazı biçimleri yalnız motorda üretilir.

`subtitle-engine translate --to <dil> [--from <dil>] [--model madlad-3b] [--json]`: stdin'den `render` ile aynı cümle JSON dizisini alır ve her cümleyi tamamen yerel modelle çevirir; `id`/`start`/`end` korunur, `text` çevrilir, `words` boş listeye döner. `--json` ile sırasıyla gerekirse `downloading_model` (`fraction` 0..1), `loading_model`, `translating` (`fraction` 0..1) aşamaları ve en sonda `translation` olayı yayınlanır; `--json` yoksa çevrilmiş SRT stdout'a yazılır. Varsayılan model MADLAD-400'dür ve kaynak dil istemez; `nllb-1.3b` için `--from` zorunludur. Desteklenmeyen dil ya da eksik kaynak dil Türkçe `error` olayı ve çıkış kodu 1 üretir. Model dosyaları ve tokenizer ilk kullanımda Hugging Face önbelleğine iner; ses ya da metin dışarı gönderilmez.

`subtitle-engine models [--json]`: model kataloğunu (`base`, `gigaam-v3`, `small`, `parakeet-v3`, `canary-1b-v2`, `medium`, `large-v3-turbo`, `large-v3`, `nllb-1.3b`, `madlad-3b`, `speaker-diarization`) tür, yaklaşık boyut, göreli hız/doğruluk, backend, lisans, desteklenen diller ve yerel önbellek durumuyla listeler; ağa çıkmaz. `disk_mb` modelin önbellekteki repo'sunun gerçek disk kullanımıdır (MB, tam sayıya yuvarlanmış; ortak VAD hariç); indirilmemişse `null`. `--json` tek satır `models` olayı yazar, aksi halde okunur tablo. For `speaker-diarization` (two repos, one of which also holds dozens of unrelated models) `disk_mb` sums only the catalog-owned files of both repos, not the repos' full size.

`subtitle-engine delete-model <id> [--json]`: indirilmiş modelin önbellekteki repo'sunu tüm revizyonları ve blob'larıyla siler; yalnız o repo klasörüne dokunur, ortak silero VAD ve diğer modeller korunur. Paylaşılan blob deposundaki (`<önbellek>/blobs/`) başka hiçbir repo'nun kullanmadığı dosyalar da temizlenir; `freed_mb` gerçekten boşalan alandır (başka modelle paylaşılan dosyalarda 0 olabilir). `--json` tek satır `{"event": "deleted", "id": "...", "freed_mb": 123}` yazar. Katalog dışı ya da indirilmemiş modelde anlaşılır Türkçe `error` olayı ve çıkış kodu 1 döner. For `speaker-diarization`, only the owned files (their snapshot entries and blobs, shared-store sweep included) are deleted from both repos, and a repo folder is removed only when nothing else is left in it. If one repo fails, the others are still cleaned and the result is a Turkish `error` saying the model was partially deleted, how many MB were freed and what remains.

stderr serbest log'dur (model yükleme uyarıları vb.); uygulamalar onu dosyaya yönlendirir, protokol değildir. Protokol değişirse `cli.py`'deki `PROTOCOL_VERSION`, `EngineEvent.swift` ve bu bölüm birlikte güncellenir. Sürüm 1'de kalır: `downloading_model` aşaması, `models` komutu, `models` olayındaki `backend`/`languages`/`disk_mb` alanları, `delete-model` komutu ve `deleted` olayı, onnx model kimlikleri, `translate` komutu, `translating` aşaması, `translation` olayı, `models` olayındaki `kind`/`license` alanları; ayrıca `diarizing` aşaması, `transcribe --diarize/--speakers` bayrakları, cümlelerde `speaker` alanı, `speaker-diarization` katalog girdisi (`kind` `"diarization"`, `backend` `"sherpa"`) ve render'daki konuşmacı çizgisi geriye uyumlu eklemelerdir (`PROTOCOL_VERSION` değişmez).

Uygulama motoru şu sırayla bulur: `SUBTITLE_ENGINE` ortam değişkeni → bundle içindeki `Contents/Resources/subtitle-engine/subtitle-engine` (PyInstaller onedir; yoksa `Contents/MacOS/subtitle-engine`) → geliştirmede `uv run --project engine subtitle-engine`.

## Komutlar

```bash
cd engine && uv sync && uv run pytest              # motor testleri
cd engine && uv run pytest -m slow                 # gerçek STT (samples/demo.mp4)
cd engine && uv run subtitle-engine transcribe ../samples/demo.mp4 -m base -l tr -o ../output -f srt vtt json
cd apps/macos && swift build && swift test         # uygulama + EngineKit testleri
cd apps/macos && ENGINE_E2E=1 swift test           # Swift → motor uçtan uca
cd apps/macos && swift run SubtitleAdderApp
scripts/package-macos.sh                           # build/Subtitle Adder.app (uv/repo gerektirmez)
```

Paketleme: `engine/subtitle-engine.spec` motoru PyInstaller ile onedir derler (`build` bağımlılık grubu), betik `.app`'i kurup içten dışa imzalar (`codesign --verify --deep --strict` ile doğrular). Varsayılan ad-hoc imzadır, yalnız derlenen makinede çalışır. `SIGN_IDENTITY="Developer ID Application: ..."` hardened runtime + `scripts/macos/engine.entitlements` (yalnız `disable-library-validation`) ile imzalar; `NOTARY_PROFILE=<notarytool profili>` notarization + staple yapar; `MAKE_DMG=1` `.dmg` üretir; `SKIP_ENGINE_BUILD=1` hazır motoru yeniden kullanır. Motor `Contents/Resources` altındadır, çünkü codesign `Frameworks`/`MacOS` altında Mach-O olmayan dosyaları reddeder; içindeki tüm Mach-O dosyalar tek tek imzalanır. Model dosyaları pakete girmez, ilk kullanımda indirilir.

Two-voice sample for diarization (macOS; English voices, alternating turns):

```bash
cd samples && i=0; while IFS='|' read -r v t; do i=$((i+1)); say -v "$v" -o t$i.aiff "$t"; afconvert -f WAVE -d LEI16@16000 -c 1 t$i.aiff t$i.wav; done <<'EOF'
Samantha (English (US))|Hello, how are you doing today? I wanted to ask you about the project.
Daniel|I am doing well, thank you. The project is going fine, we finished the first part yesterday.
Samantha (English (US))|That is great news. When do you think the second part will be ready?
Daniel|I think we will need about two more weeks. There are still a few problems to solve.
Samantha (English (US))|Okay, that sounds reasonable. Please keep me updated.
Daniel|Of course. I will send you a short report on Friday.
EOF
ffmpeg -y $(for n in 1 2 3 4 5 6; do echo -n "-i t$n.wav "; done) -filter_complex "concat=n=6:v=0:a=1[a]" -map "[a]" en.wav
ffmpeg -y -f lavfi -i color=c=black:s=320x240:r=10 -i en.wav -shortest -c:v libx264 -c:a aac dialog-en.mp4
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

- Lisans MIT (açık kaynak). Uygulamaya gömülen ya da varsayılan gelen modeller açık lisanslı olmalı; ticari olmayan lisanslı modeller (ör. NLLB) yalnız isteğe bağlı indirilir ve arayüzde işaretlenir.

- Arayüz native ve özenli; iş mantığı arayüze girmez, motordadır.
- Motor platforma özel varsayım yapmaz (sistem ffmpeg'i yok, sabit yol yok); macOS ve Windows'ta aynı çalışır.
- Python ≥3.11 (uv), Swift 6 strict concurrency, macOS 15+.
- UI metinleri ve kullanıcıya giden hata mesajları Türkçe.
- Yalnız yerel: bulut STT ya da sesi dışarı gönderen hiçbir servis yok. Ağ yalnız model indirmek için kullanılır.
- Durum ve devam noktası: `.claude/progress/STATE.md`.
