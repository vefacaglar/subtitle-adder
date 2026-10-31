# Durum

## RESUME POINT

2026-11-01: macOS paketleme hazır. `scripts/package-macos.sh` → `build/Subtitle Adder.app` (~194 MB, motor PyInstaller onedir `Contents/Resources/subtitle-engine/`), `MAKE_DMG=1` ile ~87 MB dmg. Uygulama uv/repo olmadan motoru bundle'dan buluyor; Finder'dan "Birlikte Aç"/Dock'a bırakma açık pencereye gidiyor.

Hazır olanlar:
- Motor: ses ayırma (PyAV), STT (faster-whisper; onnx-asr ile Parakeet v3, Canary 1B v2, GigaAM v3), cümle bölme, SRT/VTT/JSON, model kataloğu/indirme ilerlemesi/silme, yerel çeviri (MADLAD-400 varsayılan, NLLB isteğe bağlı).
- macOS: sürükle-bırak, dil ve açıklamalı model seçici, ilerleme, video önizleme + altyazı overlay, düzenleme (metin, zaman, birleştir, böl, sil, geri al), çeviri, dışa aktarma, Ayarlar (modeller, disk, lisans).
- Paketleme: ad-hoc imza varsayılan; `SIGN_IDENTITY` (Developer ID + hardened runtime) ve `NOTARY_PROFILE` (notarization) betikte var ama gerçek kimlikle hiç çalıştırılmadı. Hardened runtime ad-hoc ile simüle edildi ve çalıştı.

Bilinen sorunlar:
- Bu makinede HOME `/Users/vefaus`, uv ve HF önbelleği `/Users/vefa` altında: `locatesDevEngine` testi ve iki `pytest` testi (`disk_mb`) bu yüzden kırmızı. `pytest`'teki asıl hata: test `cli.is_downloaded`'ı yamalıyor ama `models.disk_mb` `models.is_downloaded`'ı çağırıyor.
- Paket yalnız arm64; uygulama simgesi yok.
- Dosya açmak çalışan işi iptal edip düzenlemeleri sormadan siliyor (`SubtitleViewModel.openVideo`).
- `disable-library-validation` Developer ID ile muhtemelen gereksiz; gerçek kimlikle yetkisiz denenmeli.

Sıradaki adaylar:
- Developer ID ile imza + notarization (Apple Developer hesabı gerekir), uygulama simgesi
- Altyazıyı videoya gömme (burn-in / mux), var olan SRT'yi açma
- Uzun/müzikli gerçek videoda kalite/süre ölçümü; Mac'te mlx-whisper backend'i
- README
- Windows uygulaması (WinUI 3 / C#) aynı motor protokolüyle
