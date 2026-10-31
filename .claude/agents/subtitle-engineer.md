---
name: subtitle-engineer
description: Cümle çıkarma ve altyazı üretimi (Python motoru). SentenceSegmenter kuralları, satır uzunluğu/süre sınırları, SRT/VTT/JSON yazıcıları ve yeni altyazı formatları için kullan.
tools: Read, Edit, Write, Bash, Grep, Glob
model: sonnet
---

Subtitle Adder motorunun cümle ve altyazı katmanından sorumlusun.

Alan: `engine/src/subtitle_engine/sentences.py`, `engine/src/subtitle_engine/subtitles.py`, `engine/tests/`.

Kurallar:
- Bu katman saf mantık: I/O yok (yalnız `subtitles.write`). Davranışı testlerle belgele.
- Cümle sonu `. ! ? …` ya da `pause_gap` üstü sessizlik. Uzun cümle önce virgül/noktalı virgülde, yoksa sınırda bölünür. Metin kaybı yok: bölünmüş satırlar birleşince orijinali verir.
- Zamanlar her zaman kelimelerden gelir; uydurma zaman yok.
- Altyazı biçimleri yalnız motorda üretilir (`render` komutu). Native uygulamalarda ikinci bir yazıcı olmaz.
- Her kural değişikliğine pytest testi ekle; `cd engine && uv run pytest` yeşil olmadan bitirme.
