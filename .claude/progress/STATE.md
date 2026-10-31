# Durum

## RESUME POINT

2026-10-31: Mimari kararı verildi: native arayüz (macOS SwiftUI, sonra Windows WinUI) + ortak Python motoru (alt süreç, JSON satır olayları; protokol CLAUDE.md'de). İlk Swift-only deneme (WhisperKit) bu yüzden kaldırıldı.

Hazır olanlar:
- `engine/`: PyAV ile ses ayırma, faster-whisper ile STT, cümle bölme, SRT/VTT/JSON, `transcribe --json` ve `render` komutları. 14 test yeşil (+1 slow).
- `apps/macos/`: EngineKit (süreç yönetimi, olay çözme, iptal, render) + SwiftUI uygulaması (sürükle-bırak, model/dil seçimi, ilerleme, cümle tablosu, dışa aktarma). 6 test yeşil; `ENGINE_E2E=1` ile gerçek motor demo videoda 3 cümle, dil `tr`.
- Uygulama açılıyor; arayüz henüz gözle kontrol edilmedi.

Sıradaki adaylar:
- Arayüzü gözle kontrol ve cilalama; altyazı düzenleme; video önizleme (AVPlayer + altyazı overlay)
- Gerçek bir videoyla (uzun, müzikli) kalite/süre ölçümü; varsayılan model kararı; Mac'te mlx-whisper backend'i (hız)
- Paketleme: motoru PyInstaller ile tek ikiliye çevirip .app içine gömme, imzalama
- Windows uygulaması (WinUI 3 / C#) aynı motor protokolüyle
