# Durum

## RESUME POINT

2026-10-31: İlk iskelet hazır. Swift Package (SubtitleCore, WhisperTranscriber, subtitle-cli, SubtitleAdderApp), 10 test yeşil. `samples/demo.mp4` (Türkçe `say` sesi) ile CLI uçtan uca çalıştı: base modelde 3 cümle, dil `tr` doğru algılandı.

Sıradaki adaylar:
- Gerçek bir videoyla (uzun, müzikli) kalite ve süre ölçümü; model varsayılanı (small vs turbo) kararı
- Uygulamada altyazı düzenleme ve video önizleme (AVPlayer + altyazı overlay)
- Xcode projesi / .app paketleme ve imzalama
- Altyazıyı videoya gömme (soft track ya da burn-in)
- Windows uygulaması için teknoloji kararı (çekirdek kuralların taşınması)
