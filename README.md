# liwinux

Linux üzerinde Android oyunları için **orkestrasyon, performans ve teşhis katmanı**.
Emülatör değildir; Waydroid'i motor olarak kullanır.

## Durum

Faz 0 (fizibilite) tamamlandı, Faz 1 (daemon iskeleti + session yaşam döngüsü) başladı.

### Doğrulanmış yığın

| Katman | Bileşen | Durum |
|---|---|---|
| Konteyner | Waydroid + LineageOS 20 (GAPPS) | ✅ |
| GPU | waydroid-nvidia → Mesa Venus → NVIDIA Vulkan | ✅ Vulkan 1.3.341 |
| GLES | ANGLE → Vulkan | ✅ GLES 3.2, ASTC |
| ARM64 | libhoudini 14 (GoogleGame) | ✅ |
| Ölçüm | Subway Surfers (arm64-v8a) | ✅ p99 5.84 ms, %0.4 jank |

## Yapı

```
crates/liw-core   Waydroid arayüzü, session süpervizörü, sağlık modeli
crates/liwd       systemd user servisi, D-Bus id.liwinux.Manager1
crates/liw        komut satırı istemcisi
scripts/          bash prototipleri (net-doctor, bench, rebuild)
scripts/poc/      Faz 0 teşhis betikleri
```

## Kullanım

```bash
cargo build --release
liw session start     # terminalden bağımsız başlatır
liw session health    # hangi göstergenin düştüğünü söyler
liw session restart   # tam kurtarma
```

## Neden session sahipliği önemli

`waydroid session start` ön planda çalışan bir süreçtir. Ölürse:

```
composer HAL → SurfaceFlinger SIGABRT → system_server → tüm uygulamalar
```

Android bundan tam kurtulamaz; sistem yeniden başlar ama varsayılan rota geri
gelmez ve ağsız bir zombi kalır. Kullanıcıya görünen belirti kök nedene hiç
benzemez. `liwd` session'ı sahiplenir, sağlığını izler ve bozulduğunda tam
döngüyle kurtarır.
