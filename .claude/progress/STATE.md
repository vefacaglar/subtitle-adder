# Durum

## RESUME POINT

2026-11-01: macOS paketleme hazır. `scripts/package-macos.sh` → `build/Subtitle Adder.app` (~194 MB, motor PyInstaller onedir `Contents/Resources/subtitle-engine/`), `MAKE_DMG=1` ile ~87 MB dmg. Uygulama uv/repo olmadan motoru bundle'dan buluyor; Finder'dan "Birlikte Aç"/Dock'a bırakma açık pencereye gidiyor.

Hazır olanlar:
- Motor: ses ayırma (PyAV), STT (faster-whisper; onnx-asr ile Parakeet v3, Canary 1B v2, GigaAM v3), cümle bölme, SRT/VTT/JSON, model kataloğu/indirme ilerlemesi/silme, yerel çeviri (MADLAD-400 varsayılan, NLLB isteğe bağlı).
- macOS: sürükle-bırak, dil ve açıklamalı model seçici, ilerleme, video önizleme + altyazı overlay, düzenleme (metin, zaman, birleştir, böl, sil, geri al), çeviri, dışa aktarma, Ayarlar (modeller, disk, lisans).
- Paketleme: ad-hoc imza varsayılan; `SIGN_IDENTITY` (Developer ID + hardened runtime) ve `NOTARY_PROFILE` (notarization) betikte var ama gerçek kimlikle hiç çalıştırılmadı. Hardened runtime ad-hoc ile simüle edildi ve çalıştı.

Bilinen sorunlar:
- huggingface_hub ≥2.4 paylaşılan blob deposu (`hub/blobs/`): `delete-model` artık oradaki sahipsiz dosyaları hf'nin kilitli süpürücüsüyle (özel API, yoksa FileLock'lu yedek) temizliyor. Başka repo'nun `blobs/` klasörü okunamazsa `scan_cache_dir` düştüğü için mesaj yanıltıcı ("indirilmemiş") ama hiçbir şey silinmiyor.
- Paket yalnız arm64. Simge yalnız paketli .app'te (`swift run` ile görünmez).
- Dosya açmak çalışan işi iptal edip düzenlemeleri sormadan siliyor (`SubtitleViewModel.openVideo`).
- `disable-library-validation` Developer ID ile muhtemelen gereksiz; gerçek kimlikle yetkisiz denenmeli.

Sıradaki adaylar:
- Developer ID ile imza + notarization (Apple Developer hesabı gerekir)
- Altyazıyı videoya gömme (burn-in / mux), var olan SRT'yi açma
- Uzun/müzikli gerçek videoda kalite/süre ölçümü; Mac'te mlx-whisper backend'i
- README
- Windows uygulaması (WinUI 3 / C#) aynı motor protokolüyle
