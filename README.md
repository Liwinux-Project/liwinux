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

## FPS fare/nişan

Nişan parmağı ekranın **dışına** çıkabilir; kenarda ortalama yoktur.
Dayanağı Waydroid'e özgü: dokunuş `/dev/input/wl_touch_events` borusuna
doğrudan yazılır, compositor zinciri atlanır ve o yolda koordinatı kırpan
hiçbir katman kalmaz. Böylece parmak hiç kaldırılmaz — "fare algılanmıyor",
"birkaç saniye ölü kalıyor" ve "aim kayıyor" belirtilerinin ortak kaynağı
ortadan kalkar.

Ayrıntı, ölçüm ve doğrulama yöntemi: [docs/fare-nisan.md](docs/fare-nisan.md)

## Takılma teşhisi

```bash
liw trace com.ForgeGames.SpecialForcesGroup2 --duration 90
```

`liw bench` "ne kadar kötü", `liw perf` "hangi kaldıraçlar açık" der.
`liw trace` **neden** sorusuna bakar: kare sunum zamanlarını, Android
günlüğünü ve host kaynaklarını AYNI saate (`CLOCK_MONOTONIC`) koyup
takılma anında ne olduğunu eşleştirir.

* Her takılmanın yanında o anki günlük olayı ve host örneği yazılır.
* **Donma** ayrı ele alınır: 60 saniye kare gelmiyorsa ortada "uzun
  aralık" yoktur, hiç aralık yoktur. Döngü "en son ne zaman yeni kare
  gördüm" diye ayrıca bakar ve donma SÜRERKEN günlüğü yakalar —
  sonradan bakmak çoğu zaman geç kalıyor, logcat halkası kanıtı
  düşürüyor.
* Tanıdığı imzalar: ağ zaman aşımı, ANR, çökme, kilit çekişmesi, GC,
  ana iş parçacığı aşımı (`Choreographer`/`Davey!`), ARM köprüsü ve
  derleme, binder, **girdi yolu kaybı**.
* Kanıt yoksa suçlama yok: "muhtemelen GPU" demek yerine açıkça
  "açıklanamadı" der ve host verisine bakar.

"Fare sisteminden mi geliyor?" sorusunun doğrudan cevabı *girdi yolu*
imzasıdır: Android dokunuş cihazımızı kaybettiyse günlüğe yazar.

## Görsel profil düzenleyici

```bash
liw profile edit com.ForgeGames.SpecialForcesGroup2
```

Oyunun ekran görüntüsünü tarayıcıda açar. Görüntü pencereye **sığdırılır**,
kaydırma gerekmez; yakınlaştırma, kaydırma ve sürüklerken açılan büyüteç var.
Koordinat her zaman KAYNAK piksele geri çevrildiği için yakınlık ne olursa
olsun yazılan değer aynı — bir kaç piksellik kayma oyunda düğmeyi ıskalatır.

* Ok tuşları tam **1 piksel** oynatır (<kbd>Shift</kbd> = 10).
* Konum piksel cinsinden de yazılabilir.
* Her kartın başlığında **tuş rozeti** var: tıkla, yeni tuşa bas, bitti.
  Ada tıklayınca satır içinde yeniden adlandırılır, **×** siler.
  Tuş ataması fiziksel tuş kodudur (evdev), klavye düzeninden bağımsız.
* Yeni bağlantı eklerken tuş hemen sorulur ve nokta görünen alanın
  ortasına konur.
* Aynı tuş iki bağlantıda kullanılırsa **yazarken** uyarır — kayıtta
  öğrenip hangisi olduğunu aramak zorunda kalmazsın.
* "Seçiliye dokun" gerçek dokunuşu Android'e gönderir — artık compositor
  atlandığı için oyun penceresinin önde olması gerekmiyor.
* "Kaydet ve uygula" keymapper'ı da yeniden başlatır; terminale gitmeden
  düzenle-dene döngüsü kurulur.
* Kayıt TOML'daki yorumları (satır sonundakiler dâhil) korur.

## Neden session sahipliği önemli

`waydroid session start` ön planda çalışan bir süreçtir. Ölürse:

```
composer HAL → SurfaceFlinger SIGABRT → system_server → tüm uygulamalar
```

Android bundan tam kurtulamaz; sistem yeniden başlar ama varsayılan rota geri
gelmez ve ağsız bir zombi kalır. Kullanıcıya görünen belirti kök nedene hiç
benzemez. `liwd` session'ı sahiplenir, sağlığını izler ve bozulduğunda tam
döngüyle kurtarır.
