# FPS fare sorunu: kök neden ve kesin çözüm

Belirti (kullanıcının tarifi): *"fare ekranın kenarına geldiğinde tekrar ortaya
ışınlanıyor; bu da farenin bazen hiç algılanmamasına, bazen 2-3 saniye
algılanmamasına, bazen de aimi kaydırmasına sebep oluyor."*

Üç belirti de **tek bir tasarım varsayımından** çıkıyor:

> "Parmak sonsuza kadar sürüklenemez, kenara gelince kaldırıp merkeze koymak
> zorundayız."

Bu varsayım Waydroid'de **YANLIŞ**. Aşağıda neden yanlış olduğu ve yerine ne
konacağı var.

---

## 1. Bugünkü zincir ve neden kaybediyor

```
evdev → liwinux motoru → uinput → libinput → KWin → wl_touch
      → Waydroid hwcomposer → /dev/input/wl_touch_events → Android EventHub
```

Kendi sanal dokunmatik ekranımızı host'ta kurup compositor'a veriyoruz.
Compositor onu Waydroid penceresine iletiyor, Waydroid de **zaten** aynı
olayları konteyner içindeki bir FIFO'ya yazıyor. Yani zincirin son halkası
hep oradaydı; biz sadece uzun yoldan gidiyoruz.

Uzun yolun bedeli:

| Halka | Ne yapıyor |
|---|---|
| çekirdek evdev | ABS değerlerini bildirilen aralığa sıkıştırır |
| libinput | dokunuşu cihaz uzayından **ekran uzayına** normalize eder — ekran dışı kalmaz |
| KWin | dokunuşu yüzey koordinatına çevirir, pencere dışını iletmez; kendi kare saatine tabidir |
| ScreenMap | pencere geometrisi matematiği (KWin betiği ile 5 sn'de bir yoklanıyor) |

Sonuç: **parmak ekran dışına çıkamaz.** Bu yüzden motor kenarda
kaldır-ortala-indir yapmak zorunda kalıyor ve tüm karmaşa (gecikmeli iniş,
devir teslim, doğrusal olmayan ölçekleme, boşta ortalama) bu tek kısıtı
saklamak için var.

---

## 2. Waydroid'in girdi yolu (doğrulandı)

Waydroid, Android'in `EventHub`'ını yamalar
(`anbox-patches/frameworks/native/0006-EventHub-Add-wayland-inputs-support.patch`).
Konteyner içinde üç **isimlendirilmiş boru** dinlenir:

| Yol | EventHub cihaz adı | Sınıf |
|---|---|---|
| `/dev/input/wl_touch_events` | `wayland_touch` | `TOUCH_MT` + `INPUT_PROP_DIRECT` |
| `/dev/input/wl_pointer_events` | `wayland_pointer` | `CURSOR` |
| `/dev/input/wl_keyboard_events` | `wayland_keyboard` | `KEYBOARD` |

Bu makinede doğrulandı:

* `libinputreader.so` içinde `/dev/input/wl_touch_events`, `wayland_touch`,
  `waydroid.display_width`, `waydroid.display_height` dizeleri var.
* FIFO'yu **hwcomposer** kurar (`wayland-hwc.cpp`):
  `mkfifo(..., 0660)` + `chown(..., 1000, 1000)` → sahibi `system:system`.
* Ekran hotplug'ında FIFO **silinip yeniden yaratılır** — açık fd
  sahipsiz kalır, yeniden açmak gerekir.

### Boru protokolü

Ham `struct input_event` dizisi (x86_64'te kayıt başına 24 bayt),
`CLOCK_MONOTONIC` zaman damgasıyla. hwcomposer'ın birebir ürettiği sıra:

```c
// iniş / hareket (aynı sıra)
ABS_MT_SLOT        = slot
ABS_MT_TRACKING_ID = slot          // iniş ve harekette aynı
ABS_MT_POSITION_X  = x             // ANDROID EKRAN PİKSELİ
ABS_MT_POSITION_Y  = y
ABS_MT_PRESSURE    = 50
SYN_REPORT         = 0
// kalkış
ABS_MT_SLOT        = slot
ABS_MT_TRACKING_ID = -1
SYN_REPORT         = 0
```

`BTN_TOUCH` YOK, `ABS_X`/`ABS_Y` YOK. Koordinat uzayı doğrudan
`waydroid.display_width` × `waydroid.display_height` (burada 2560×1440);
EventHub eksen bilgisini bu property'lerden **uyduruyor**, cihazdan
sormuyor (`getAbsoluteAxisInfo` içindeki `location == "wayland"` dalı).

**Bölünmezlik:** her kare TEK `write()` ile gitmeli. hwcomposer da aynı
boruya yazıyor; POSIX yalnızca `PIPE_BUF` (4096 bayt) altındaki yazmaların
bölünmezliğini garanti eder. 4096 / 24 = 170 olay; bir karemiz en fazla
~60 olay, güvenli.

---

## 3. Asıl bulgu: bu yolda koordinat KIRPILMIYOR

Üç bağımsız katman zinciri kırpmıyor:

1. **Çekirdek yok.** FIFO bir borudur; evdev sürücü katmanı hiç devrede
   değil. `input_handle_abs_event` çalışmıyor, `ABS` aralık kırpması yok.
   *(uinput yolunda bu mümkün değildi.)*

2. **`TouchInputMapper::cookPointerData()` kırpmıyor.** Android 13 kaynağı:
   `mAffineTransform.applyTo()` + `rotateAndScale()`, sonra doğrudan
   `AMOTION_EVENT_AXIS_X/Y`. Yüzey sınırı testi yok; dosyadaki tek
   `clamp` `clampResolution()` ve o dokunuş **boyutu** eksenleri için.

3. **`InputDispatcher` MOVE'da pencereyi yeniden seçmiyor.**
   `findTouchedWindowTargetsLocked()` yalnızca `ACTION_DOWN` /
   `ACTION_POINTER_DOWN` için hedef arar (`newGesture` dalı). MOVE
   mandallanmış `tempTouchState`'e gider. Tek istisna `SLIPPERY` bayraklı
   pencereler — oyunlar bunu koymaz ve o dal zaten `pointerCount == 1`
   şartına bağlı.

### Sonuç

> **Bir dokunuş oyun penceresi içinde İNDİĞİ sürece, sonraki hareketleri
> ekranın dışına çıksa bile aynı pencereye ulaşır.**

Bu, kenarda ortalama zorunluluğunu **tamamen** kaldırır. Nişan parmağı bir
kez iner ve nişan bırakılana kadar sınırsız düzlemde gezer.

Neredeyse tüm Android FPS'leri bakışı `delta = konum - önceki_konum` ile
hesaplar; mutlak konumu yalnızca `ACTION_DOWN` anında, "bu dokunuş bakış
bölgesinde mi" testinde kullanır. İniş doğru yerde olduğu sürece parmağın
sonradan nereye gittiği oyunu ilgilendirmez.

---

## 4. Üç belirtinin kök nedenleri

### 4.1 "Bazen hiç algılanmıyor"

**a) Nişan parmağı sol yarıya kayıyor.** Profilde `origin = (0.50, 0.50)`
ve `recenter_margin = 0.03`, yani parmak x ∈ [0.03, 0.97] boyunca geziyor.
FPS oyunlarının çoğunda bakış bölgesi testi `x > genişlik/2`. Sıfırlama
sonrası parmak sol yarıda inerse oyun onu **hareket joystick'i** sayar:
bakış ölür, üstüne karakter yana yürür. `origin` tam sınırın üstünde
olduğu için bu her sıfırlamada kura çekiyor.

**b) İşaretçi havuzu sızıntısı.** Havuz dolunca `on_mouse` işaretçi alamaz
ve fare tamamen ölür (`reconcile_pointers` bunu onarmak için var, yani
gerçekte yaşanmış).

### 4.2 "2-3 saniye algılanmıyor"

`engine.rs`'te gecikmeli inişte **kare birleşmesi hatası** var:

```rust
if let Some(t) = self.aim_down_at {
    if now_ms >= t {
        self.aim_down_at = None;
        acts.push(TouchAction::Down { id, at: origin });   // iniş
    }
    if self.aim_down_at.is_some() { return ...; }          // artık None → düşer
}
...
} else if self.aim_accum != (0.0, 0.0) {
    acts.extend(self.on_mouse(ax, ay));                    // AYNI karede hareket
}
```

İniş ve hareket aynı `dispatch()` çağrısına, dolayısıyla **aynı
`SYN_REPORT`'a** düşüyor. Kodun başka yerinde defalarca uyarılan hata tam
olarak bu. Üstelik `aim_accum`, gecikme boyunca (12 ms + 5 ms tick
granülü) birikmeye devam ettiği için iniş doğrudan birikmiş konumda
gerçekleşiyor: oyun sıfır delta görüyor, o dönüş kayboluyor.

Hızlı çevirmede bu döngüye giriyor — her sıfırlama bir sonrakini besliyor
ve kullanıcı saniyeler süren ölü bölge yaşıyor.

`idle_recenter()` de `Up` + `Down`'u tek dizide döndürüyor; aynı kare
birleşmesi orada da var.

### 4.3 "Aimi kaydırıyor"

**a) `nonlinear = true`.** Hassasiyet `sqrt(min_d / d)` ile ölçekleniyor;
parmak merkezden uzaklaştıkça **3 kata kadar** düşüyor. Yani aynı fare
hareketi, parmağın görünmeyen konumuna göre farklı açı döndürüyor. FPS'te
kas hafızası bunun üstüne kurulamaz — belirti tam olarak "aim kayıyor".

**b) `reseat_point` ters uca oturtuyor.** Sağa dönerken parmak sol kenara
konuyor. Bu ekran genişliğini kazandırıyor ama iniş noktasını oyunun
hareket pedi bölgesine sokuyor (4.1a).

**c) Taşan hareket atılıyor.** Sınırı aşan miktar bilinçli olarak
düşürülüyor (yorumda gerekçesi var) — döngüyü önlüyor ama her sıfırlamada
bir miktar dönüş kaybediliyor.

---

## 5. Çözüm

### 5.1 Enjeksiyon yolunu kısalt

```
evdev → liwinux motoru → /proc/<konteyner>/root/dev/input/wl_touch_events
```

`uinput`, `libinput`, `KWin`, `wl_touch` ve `ScreenMap` zincirden çıkar.

**Ayrıcalık:** FIFO `system:system 0660`; yazmak için root gerekir.
`liwd-helper` zaten root çalışıyor. Doğru tasarım, helper'ın FIFO'yu açıp
**dosya tanıtıcısını D-Bus üzerinden geri vermesi** (`zvariant::OwnedFd`).
Böylece:

* yetki polkit'te bir kez sorulur,
* 200 Hz'lik yazma trafiği IPC'den geçmez, `liwd` doğrudan fd'ye yazar,
* helper genel amaçlı bir yazma arayüzü açmaz — tek bir yazma-tanıtıcısı verir.

Konteynerin mount namespace'ine girmek için `nsenter` gerekmiyor: root,
`/proc/<pid>/root/...` üzerinden doğrudan açabilir.

**Dikkat:** hotplug'da FIFO yeniden yaratılıyor; `waydroid.display_width`
değiştiğinde veya yazma başarısız olduğunda fd yeniden istenmeli.

### 5.2 Sınırsız nişan (`unbounded aim`)

Nişan parmağı için:

* İniş **bir kez**, oyunun bakış bölgesinin ortasında.
* Sonrasında yalnızca `Move`. Kalkış yok, ortalama yok, devir teslim yok,
  gecikmeli iniş yok.
* Hassasiyet **sabit** (`nonlinear` kapalı) — 1:1 his.
* Konum `f64` olarak sınırsız tutulur; yalnızca taşmaya karşı geniş bir
  emniyet kutusu (ör. ±8 ekran) vardır ve oraya varılırsa **fare
  durduğunda** sessizce sıfırlanır (görülmez).

Böylece 4.1, 4.2 ve 4.3'ün tamamı ortadan kalkar: kaldırılacak parmak yok,
beklenecek gecikme yok, değişen hassasiyet yok.

### 5.3 Profil düzeltmeleri (SFG2)

* `origin` ekranın ortasına ama tam sınıra DEĞİL: `x = 0.5004`, `y = 0.50`
  (2560'ta piksel 1281 — merkezin bir piksel sağı).

  Sınırsız kipte iniş bir kez olduğu için bu nokta yalnızca "oyun bunu
  bakış sayıyor mu" sorusunu belirliyor; gerisi delta. Tam `0.50`
  kullanılmamalı: `x > genişlik/2` testini `>` ile yazan bir oyunda
  `1280 > 1280` yanlış olur, parmak sol yarıya düşer ve oyun onu hareket
  pedi sanar — nişan komple ölür. Bir piksel sağı gözle ortadan ayırt
  edilemez ve bu riski sıfırlar.
* `nonlinear = false`, `handoff = false`, sıfırlama alanları anlamsızlaşır.
* Oyun içi bakış hassasiyetini yükseltip `sensitivity`'yi düşürmek en iyi
  hissi verir; sınırsız modda zorunlu değil ama sayısal aralığı küçük
  tutar.
* `persist.waydroid.fake_touch` **açık kalmalı** (ilk taslakta yanlış
  yazmıştım). Kaynağı okuduktan sonra netleşti:

  `0016-Fake-touch-inputs-for-select-apps.patch`, `ViewRootImpl`'de
  `deliverInputEvent` sırasında yalnızca şunu yapıyor:

  ```java
  if (mFakeClickAsTouch && q.mEvent instanceof MotionEvent) {
      int action = ev.getAction();
      if (action == ACTION_MOVE || ACTION_DOWN || ACTION_UP)
          ev.setSource(4098);   // SOURCE_TOUCHSCREEN
  }
  ```

  Yani **yeni dokunuş üretmiyor**, var olan olayın kaynağını yeniden
  etiketliyor. Bizim boruya yazdıklarımız `wayland_touch` cihazından
  geldiği için zaten `SOURCE_TOUCHSCREEN`; `setSource(4098)` üzerimizde
  işlemsiz. Çakışma yok.

  Oyun kipinde fare zaten kilitli, yani Waydroid'e hiç fare olayı
  gitmiyor. Oyun kipi kapalıyken ise fake_touch gerekiyor: menü öğeleri
  fareyle ancak onunla tıklanabiliyor.

  **Oyun kipiyle birlikte açıp kapatmak işe yaramaz.** `mFakeClickAsTouch`
  `final` bir alan ve `ViewRootImpl` **kurucusunda** okunuyor; değer
  pencere yaratılırken mandallanıyor. Çalışan oyunu etkilemez, yalnızca
  sonradan açılan pencereler yeni değeri görür — tutarsız ve gizli bir
  davranış olurdu.

### 5.4 Doğrulama

`scripts/poc/fifo-touch.py` iki iddiayı da sınar (root ile çalıştırılır).
Aşama 1 iz bırakıyorsa FIFO yolu çalışıyor; aşama 2'de dönüş devam
ediyorsa kırpma yok ve sınırsız nişan uygulanabilir.

Oyun ekran dışını tolere etmiyorsa geri çekilme planı: sınırsız yerine
**geniş** kutu (ör. 3 ekran) — sıfırlama sıklığı yine 3 kat düşer ve
sıfırlamalar 5.2'deki "yalnızca fare dururken" kuralına bağlanır.

---

## 6. Karşılaştırma: mevcut projeler

| Proje | Enjeksiyon | Kenar sorunu |
|---|---|---|
| **XtMapper** | `app_process` kabuk servisi → `InputManager.injectInputEvent` | Aynı sorun var; doğrusal olmayan ölçekleme + gecikmeli sıfırlama ile *hafifletiyor*. Bu projedeki `nonlinear`/`reset_delay_ms` oradan alınmış. |
| **waydroid-helper** | scrcpy protokolü (TCP + `app_process` sunucu) | Aim widget'ı var; aynı mutlak dokunuş kısıtına tabi. |
| **scrcpy** | `InputManager` enjeksiyonu | Nişan kipi yok; 1:1 mutlak eşleme. |
| **liwinux (yeni)** | Waydroid FIFO'suna doğrudan yazma | Kırpma olmadığı için sorun **oluşmuyor** — hafifletme gerekmiyor. |

Fark şu: diğerlerinin hepsi Android **framework** seviyesinden enjekte
ediyor, orada koordinatlar zaten ekran uzayına oturmuş oluyor. Waydroid'in
FIFO'su ise `InputReader`'ın **altında**; kırpmanın yapılacağı katmanların
hiçbiri devrede değil. Bu, Waydroid'e özgü bir avantaj.


---

## 7. Uygulama durumu

Yapıldı:

* `crates/liw-core/src/input/wl_touch.rs` — boruya doğrudan yazan arka uç.
* `Norm::unclamped` / `is_offscreen` — ekran dışı koordinat taşıyabilen tip.
* `Binding::Aim` içinde `unbounded` (varsayılan açık) ve `safety_span`
  (varsayılan 32 ekran).
* `Engine::set_offscreen_ok` — sınırsız kip yalnızca kırpmayan arka uçta
  açılır; uinput yolunda motor kendiliğinden sınırlı kipe düşer.
* `liwd-helper` → `OpenTouchPipe`: boruyu açıp yazma tanıtıcısını D-Bus
  üzerinden devreder (polkit: `id.liwinux.helper.touch-pipe`).
* `liwd` ve `liw keymap run` boruyu ister, alamazsa GÜRÜLTÜLÜ biçimde
  uinput'a düşer.
* `liw keymap poke` varsayılan olarak boruyu kullanır ve 0..1 dışı
  koordinat kabul eder — iddianın doğrulama aracı.
* SFG2 profili yeniden yazıldı.

### İki kare birleşmesi hatası da düzeltildi

Bunlar sınırsız kipten bağımsız, sınırlı yolda da yanlıştı:

1. `tick()` gecikmeli inişi yaparken aynı karede biriken hareketi de
   uyguluyordu → iniş + hareket tek `SYN_REPORT`, delta kayboluyor.
   Artık iniş kendi karesinde yalnız kalıyor.
2. `idle_recenter()` `Up` ve `Down`'u tek dizide döndürüyordu → aynı
   birleşme. Artık kalkış hemen, iniş bir sonraki karede.

### Doğrulanmadan kalan

Boru yolu **uçtan uca çalışırken ölçülmedi**: boruya yazmak root
gerektiriyor ve kurulu `liwd-helper` henüz eski sürüm. Dayanak şu ana
kadar kaynak ve ikili inceleme:

* `libinputreader.so` içinde `/dev/input/wl_touch_events` + `wayland_touch`
  + `waydroid.display_width/height` dizeleri (bu makinede),
* Waydroid'in `EventHub` yaması ve `wayland-hwc.cpp` yazıcı tarafı,
* Android 13 `TouchInputMapper.cpp` / `InputDispatcher.cpp` kaynağı.

Sıradaki adım: helper'ı güncelle, sonra

```
liw keymap poke 0.72 0.5 --to 3.0,0.5 --hold 900
```

`--to 3.0` ekranın üç katı. Dokunuş izi ekran kenarında durmayıp oyun
dönmeye devam ediyorsa iddia doğrulanmıştır.
