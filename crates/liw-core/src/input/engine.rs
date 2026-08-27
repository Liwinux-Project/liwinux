//! Eşleme motoru: host girdisi → dokunuş eylemleri.
//!
//! Saf ve senkron tutulmuştur: içeride I/O yok, zaman kaynağı dışarıdan
//! verilir. Böylece tüm davranış birim testlerle doğrulanabilir — bir
//! keymapper'da "his" hatalarının çoğu durum makinesinde saklanır ve
//! elle denemekle yakalanamaz.

use super::profile::{Binding, Easing, Profile, Trigger};
use super::touch::{Norm, PointerPool, TouchAction, MAX_POINTERS};
use std::collections::HashSet;

/// Motora gelen ham girdi olayı.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    Press(Trigger2),
    Release(Trigger2),
    /// Bağıl fare hareketi (piksel).
    MouseMove { dx: f32, dy: f32 },
}

/// `Trigger`ın Copy olabilen ikizi (motor içi kullanım).
pub type Trigger2 = TriggerKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TriggerKind {
    Key(u16),
    MouseLeft,
    MouseRight,
    MouseMiddle,
    WheelUp,
    WheelDown,
}

impl From<&Trigger> for TriggerKind {
    fn from(t: &Trigger) -> Self {
        match t {
            Trigger::Key(k) => TriggerKind::Key(*k),
            Trigger::MouseLeft => TriggerKind::MouseLeft,
            Trigger::MouseRight => TriggerKind::MouseRight,
            Trigger::MouseMiddle => TriggerKind::MouseMiddle,
            Trigger::WheelUp => TriggerKind::WheelUp,
            Trigger::WheelDown => TriggerKind::WheelDown,
        }
    }
}

/// Devam eden bir kaydırma jesti.
///
/// Kaydırma "ateşle ve unut"tur: tuşa basınca tam jest oynar, tuşun ne kadar
/// basılı tutulduğu önemli değildir. Oyunlar kaydırmayı ara hareketlerden
/// tanır; tek sıçrama jest sayılmaz.
#[derive(Debug, Clone)]
struct ActiveSwipe {
    binding: String,
    group: Option<String>,
    easing: Easing,
    id: u8,
    from: Norm,
    to: Norm,
    start_ms: u64,
    duration_ms: u64,
    /// Son gönderilen ara adım; aynı konumu tekrar göndermemek için.
    last_step: u32,
    /// Son GÖNDERİLEN konum. Öne yüklü eğrilerde sondaki adımlar birkaç
    /// piksel oynar; bunlar bilgi taşımaz ve parmağın "takıldığı" izlenimi
    /// vererek jestin basılı tutma sanılmasına yol açabilir.
    last_sent: Norm,
}

/// Ardışık iki adım bundan daha yakınsa ara adım gönderilmez.
/// 0.002 normalize ≈ 2540 piksel genişlikte ~5 piksel.
const MIN_STEP_DELTA: f32 = 0.002;

/// Devir teslim için ikinci parmağın havuz anahtarı.
fn second_key(name: &str) -> String { format!("{name}\u{0}2") }

/// Sınırsız nişanda bir adım: kırpma YOK, yalnızca emniyet kutusu.
///
/// Kutunun amacı his değil taşma koruması. Konum f32, tel üzerindeki
/// değer i32; sınırsız bırakmak yeterince uzun oyunda hassasiyeti
/// aşındırırdı. Kutuya dayanılırsa nişan orada takılır ama bu, fare ilk
/// durduğunda `idle_recenter` tarafından sessizce onarılır.
fn free_step(pos: Norm, origin: Norm, safety_span: f32, dx: f32, dy: f32) -> Norm {
    let lim = safety_span.max(1.0);
    Norm::unclamped(
        (pos.x + dx).clamp(origin.x - lim, origin.x + lim),
        (pos.y + dy).clamp(origin.y - lim, origin.y + lim),
    )
}

/// Kaydırma kaç ara adıma bölünsün. Az olursa jest tanınmaz, çok olursa
/// gereksiz olay üretilir. 12 adım 80ms'de ~7ms aralık demek — 144Hz'de bile yeterli.
const SWIPE_STEPS: u32 = 12;

pub struct Engine {
    profile: Profile,
    pool: PointerPool,
    held: HashSet<TriggerKind>,
    swipes: Vec<ActiveSwipe>,
    now_ms: u64,
    /// Ekran en/boy oranı (genişlik / yükseklik).
    ///
    /// Normalize koordinatlar eksen başına ölçeklidir: 2560x1440'ta
    /// 0.085 yatayda 218, dikeyde 122 piksel eder. Düzeltilmezse joystick
    /// elips olur — A/D, W/S'den 1.78 kat uzağa gider.
    aspect: f32,
    /// Yeniden ortalama sonrası bir sonraki kareye ertelenen fare hareketi.
    ///
    /// Down ve Move AYNI SYN_REPORT içinde giderse oyun tek kare görür ve
    /// parmak doğrudan hedefte belirir; yeni dokunuşun önceki konumu
    /// olmadığı için delta hesaplanamaz ve dönüş kaybolur.
    aim_pending: Option<(f32, f32)>,
    /// Son fare hareketinin zamanı; boşta yeniden ortalama için.
    aim_last_move_ms: u64,
    /// Kalkıştan sonra inişin yapılacağı zaman (ms) — gecikmeli sıfırlama.
    aim_down_at: Option<u64>,
    /// Biriken fare hareketi; kare başına bir kez uygulanır.
    ///
    /// Fare 1000 Hz örnekleniyor ama gerçek dokunmatik ekranlar 60-240 Hz
    /// raporlar. Her fare olayında bir Move göndermek Android'in girdi
    /// hattını sele veriyor: olaylar toplu işleniyor, Up/Down çifti selin
    /// içinde sırasını kaybediyor ve dönüş kenarda takılıyor.
    /// Biriktirip kare başına tek hareket göndermek gerçek donanımın
    /// davranışıdır.
    aim_accum: (f32, f32),
    /// Devir teslim için önceden indirilmiş ikinci parmağın konumu.
    ///
    /// Birincisi kenara varmadan inip onunla BİRLİKTE hareket eder. Devir
    /// anında ekranda hareket eden bir parmak kalır, dönüş kesilmez.
    aim_second: Option<Norm>,
    /// İlk basışta merkeze inen joystick'in bir sonraki karede taşınması.
    ///
    /// Gerçek oyuncu joystick'in ortasına basıp sürükler. Parmağı doğrudan
    /// kenara indirmek bazı oyunlarda hareketi başlatmıyor.
    joystick_pending: Option<String>,
    /// Aim modunda mevcut parmak konumu.
    aim_pos: Option<Norm>,
    /// Motor etkin mi. Kapalıyken hiçbir olay üretilmez ama takılı
    /// parmaklar bırakılır — aksi halde oyunda parmak asılı kalır.
    enabled: bool,
    /// Arka uç ekran DIŞI koordinat taşıyabiliyor mu.
    ///
    /// Profildeki `unbounded` yalnız başına yetmez: uinput yolunda
    /// libinput ve KWin koordinatı ekrana sıkıştırır, parmak kenarda
    /// takılır ve nişan tamamen ölür. Bu yüzden karar arka uca ait ve
    /// varsayılan KAPALI — bilinmezken sınırlı kip her yolda çalışır.
    offscreen_ok: bool,
}

impl Engine {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile, pool: PointerPool::new(), held: HashSet::new(),
            swipes: Vec::new(), now_ms: 0,
            aspect: 16.0 / 9.0,
            aim_pending: None, joystick_pending: None,
            aim_last_move_ms: 0,
            aim_accum: (0.0, 0.0),
            aim_down_at: None,
            aim_second: None,
            aim_pos: None, enabled: true,
            offscreen_ok: false,
        }
    }

    pub fn profile(&self) -> &Profile { &self.profile }

    /// Ekran en/boy oranını ayarlar. Joystick dairesinin gerçekten daire
    /// olması buna bağlı.
    pub fn set_aspect(&mut self, w: u32, h: u32) {
        if w > 0 && h > 0 { self.aspect = w as f32 / h as f32; }
    }
    pub fn is_enabled(&self) -> bool { self.enabled }

    /// Arka ucun ekran dışı koordinat taşıyabildiğini bildirir.
    ///
    /// Yalnızca Waydroid'in dokunuş borusuna doğrudan yazan arka uç için
    /// doğrudur; ayrıntı `docs/fare-nisan.md`. Profilde `unbounded = true`
    /// olsa bile bu açılmadan sınırsız nişan devreye girmez.
    pub fn set_offscreen_ok(&mut self, ok: bool) { self.offscreen_ok = ok; }

    /// Sınırsız nişan şu an gerçekten etkin mi (teşhis ve test).
    pub fn aim_is_unbounded(&self) -> bool {
        self.offscreen_ok && self.profile.bindings.values().any(|b|
            matches!(b, Binding::Aim { unbounded: true, .. }))
    }

    /// Motoru açar/kapatır. Kapatırken tüm parmakları kaldırır.
    #[must_use = "kapatma sırasında üretilen UP eylemleri gönderilmezse                   parmaklar ekranda kalır"]
    pub fn set_enabled(&mut self, on: bool) -> Vec<TouchAction> {
        if self.enabled == on { return Vec::new(); }
        self.enabled = on;
        if !on {
            self.held.clear();
            self.aim_pos = None;
            self.aim_pending = None;
            self.aim_accum = (0.0, 0.0);
            self.aim_down_at = None;
            self.aim_second = None;
            self.joystick_pending = None;
            self.swipes.clear();
            self.pool.release_all()
        } else { Vec::new() }
    }

    /// Bir tetikleyiciyi hangi bağlantının kullandığını bulur.
    fn owner(&self, t: TriggerKind) -> Option<(&str, &Binding)> {
        self.profile.bindings.iter()
            .find(|(_, b)| b.triggers().iter().any(|x| TriggerKind::from(x) == t))
            .map(|(n, b)| (n.as_str(), b))
    }

    /// Zamanı ilerletir ve devam eden jestlerin ara adımlarını üretir.
    ///
    /// Motor saf kalsın diye zaman DIŞARIDAN verilir; böylece jest zamanlaması
    /// gerçek saate ihtiyaç duymadan test edilebilir.
    #[must_use = "üretilen dokunuş eylemleri arka uca GÖNDERİLMELİ;                   atılırsa parmak ekranda asılı kalır"]
    pub fn tick(&mut self, now_ms: u64) -> Vec<TouchAction> {
        self.now_ms = now_ms;
        if !self.enabled { return Vec::new(); }
        let mut acts = Vec::new();

        // Ertelenmiş joystick yönü: Down'dan SONRAKİ karede uygulanır.
        if let Some(name) = self.joystick_pending.take() {
            acts.extend(self.recompute_joystick(&name));
        }
        // Sızıntı onarımı ÖNCE: havuz dolarsa nişan işaretçi alamaz ve
        // fare tamamen ölür.
        acts.extend(self.reconcile_pointers());

        // Gecikmeli iniş: kalkıştan sonra Android'in dokunuşu gerçekten
        // bitirmesi için zaman tanınır. Aynı karede göndermek oyunun
        // ışınlanma görmesine yol açıyor.
        if let Some(t) = self.aim_down_at {
            if now_ms >= t {
                self.aim_down_at = None;
                if let Some((name, Binding::Aim { origin, .. })) =
                    self.profile.bindings.iter()
                        .find(|(_, b)| matches!(b, Binding::Aim { .. }))
                        .map(|(n, b)| (n.clone(), b.clone()))
                {
                    match self.pool.acquire(&name) {
                        Some(id) => {
                            self.aim_pos = Some(origin);
                            acts.push(TouchAction::Down { id, at: origin });
                        }
                        None => tracing::error!(
                            kullanımda = self.pool.active_count(),
                            "nişan inişi yapılamadı — havuz dolu"),
                    }
                }
            }
            // HER İKİ durumda da nişan tarafını atlayıp çıkıyoruz —
            // ama joystick ve kaydırmaların bekleyen işleri yapılmalı,
            // yoksa yürürken nişan sıfırlanınca hareket duruyor.
            //
            // İniş bu tick'te YAPILDIYSA da çıkmak ŞART. Devam etseydik
            // biriken fare hareketi aynı dizide bir `Move` üretirdi;
            // ikisi tek SYN_REPORT'a düşer, dokunuş doğrudan hedefte
            // belirir ve oyun delta hesaplayamaz — o dönüş kaybolur.
            // Hızlı çevirmede bu kendini besleyip saniyelerce ölü bölge
            // üretiyordu. Biriken hareket bir sonraki kareye kalır.
            return self.tick_gestures(now_ms, acts);
        }

        // SIRA ÖNEMLİ: ertelenen ÖNCE uygulanır.
        //
        // Tersi olursa yeniden yerleşmenin ürettiği ertelenmiş hareket aynı
        // tick içinde tüketilir; Down ve Move tek SYN_REPORT'ta birleşir,
        // aynı slotta son konum kazanır ve oyun yine ışınlanma görür.
        // Ertelenen varken biriken bir sonraki kareye kalır — kaybolmaz.
        if let Some((dx, dy)) = self.aim_pending.take() {
            // Ertelenen ile o sırada BİRİKEN birleştirilir: ikisi de aynı
            // slotta tek Move olur. Down önceki tick'te gitti, çakışma yok.
            //
            // Ayrı uygulamak boşluğu iki tick'e çıkarıyordu: bir tick
            // ertelenen için, bir tick de o sırada birikmiş olan için.
            let (ax, ay) = std::mem::take(&mut self.aim_accum);
            acts.extend(self.apply_aim_delta(dx + ax, dy + ay));
        } else if self.aim_accum != (0.0, 0.0) {
            // Biriken fare hareketi: kare başına TEK Move.
            let (ax, ay) = std::mem::take(&mut self.aim_accum);
            acts.extend(self.on_mouse(ax, ay));
        } else {
            // BOŞTA yeniden ortalama: fare durduğunda parmağı merkeze çek.
            //
            // Ortalamanın bedeli bir karelik dönüş boşluğu. Hareket
            // sırasında yapılırsa duraklama olarak hissedilir; durakta
            // yapılırsa hiç fark edilmez. Böylece hızlı çevirmelerde
            // ortalama gerekliliği baştan azalır.
            acts.extend(self.idle_recenter());
        }

        let mut finished: Vec<String> = Vec::new();

        for sw in &mut self.swipes {
            let elapsed = now_ms.saturating_sub(sw.start_ms);
            let step = if sw.duration_ms == 0 { SWIPE_STEPS } else {
                ((elapsed * SWIPE_STEPS as u64) / sw.duration_ms).min(SWIPE_STEPS as u64) as u32
            };
            if step == sw.last_step { continue; }
            sw.last_step = step;
            // Eğri, ZAMAN ilerlemesini MESAFE oranına çevirir. Adım sayısı
            // değişmez; değişen, her adımda ne kadar yol katedildiğidir.
            let t = sw.easing.apply(step as f32 / SWIPE_STEPS as f32);
            let at = Norm::new(
                sw.from.x + (sw.to.x - sw.from.x) * t,
                sw.from.y + (sw.to.y - sw.from.y) * t,
            );
            // Son adım HER ZAMAN gönderilir: jest hedefe varmalı.
            let is_final = step >= SWIPE_STEPS;
            let dx = at.x - sw.last_sent.x;
            let dy = at.y - sw.last_sent.y;
            let moved_enough = (dx * dx + dy * dy).sqrt() >= MIN_STEP_DELTA;
            if is_final || moved_enough {
                acts.push(TouchAction::Move { id: sw.id, at });
                sw.last_sent = at;
            }
            if is_final {
                acts.push(TouchAction::Up { id: sw.id });
                finished.push(sw.binding.clone());
            }
        }
        for name in finished {
            self.pool.release(&name);
            self.swipes.retain(|s| s.binding != name);
        }
        acts
    }

    /// Yürürken nişan sıfırlanırsa hareket DURMAMALI.
    ///
    /// Gerçek hata: gecikmeli iniş beklenirken tick erken çıkıyordu ve
    /// joystick'in bekleyen yönü hiç uygulanmıyordu.
    /// Sahipsiz kalmış işaretçileri bulur ve bırakır.
    ///
    /// `Tap`/`Toggle` bağlantıları basışta işaretçi alır, bırakışta verir.
    /// Bırakma olayı KAYBOLURSA (oyun kipi geçişi, odak değişimi, kilit
    /// alma/bırakma sırasında olabiliyor) işaretçi sonsuza kadar tutulu
    /// kalır. Birkaç sızıntıdan sonra havuz dolar ve nişan yeni işaretçi
    /// alamaz — fare TAMAMEN ölür. Gerçekte yaşandı.
    ///
    /// Bu yüzden her tick'te tutulan işaretçilerle basılı tuşlar
    /// karşılaştırılır; karşılığı olmayan bırakılır.
    fn reconcile_pointers(&mut self) -> Vec<TouchAction> {
        let mut acts = Vec::new();
        let names: Vec<String> = self.profile.bindings.keys().cloned().collect();
        for name in names {
            // Nişan, joystick ve süren kaydırmalar kendi ömrünü yönetir.
            let Some(b) = self.profile.bindings.get(&name) else { continue };
            let expects_hold = match b {
                Binding::Tap { trigger, .. } | Binding::Toggle { trigger, .. } =>
                    Some(TriggerKind::from(trigger)),
                _ => None,
            };
            let Some(t) = expects_hold else { continue };
            if self.pool.get(&name).is_some() && !self.held.contains(&t) {
                if let Some(id) = self.pool.release(&name) {
                    tracing::warn!(bağlantı = %name,
                        "sahipsiz işaretçi bırakıldı (kayıp tuş bırakma olayı)");
                    acts.push(TouchAction::Up { id });
                }
            }
        }
        acts
    }

    /// Nişan dışındaki bekleyen işler (joystick yönü, kaydırma adımları).
    fn tick_gestures(&mut self, now_ms: u64, mut acts: Vec<TouchAction>) -> Vec<TouchAction> {
        if let Some(name) = self.joystick_pending.take() {
            acts.extend(self.recompute_joystick(&name));
        }
        let mut finished: Vec<String> = Vec::new();
        for sw in &mut self.swipes {
            let elapsed = now_ms.saturating_sub(sw.start_ms);
            let step = if sw.duration_ms == 0 { SWIPE_STEPS } else {
                ((elapsed * SWIPE_STEPS as u64) / sw.duration_ms)
                    .min(SWIPE_STEPS as u64) as u32
            };
            if step == sw.last_step { continue; }
            sw.last_step = step;
            let t = sw.easing.apply(step as f32 / SWIPE_STEPS as f32);
            let at = Norm::new(
                sw.from.x + (sw.to.x - sw.from.x) * t,
                sw.from.y + (sw.to.y - sw.from.y) * t,
            );
            let is_final = step >= SWIPE_STEPS;
            let dx = at.x - sw.last_sent.x;
            let dy = at.y - sw.last_sent.y;
            if is_final || (dx * dx + dy * dy).sqrt() >= MIN_STEP_DELTA {
                acts.push(TouchAction::Move { id: sw.id, at });
                sw.last_sent = at;
            }
            if is_final {
                acts.push(TouchAction::Up { id: sw.id });
                finished.push(sw.binding.clone());
            }
        }
        for name in finished {
            self.pool.release(&name);
            self.swipes.retain(|s| s.binding != name);
        }
        acts
    }

    /// Devam eden bir jest var mı? Varsa çağıran `tick`i sık çağırmalı.
    pub fn has_pending(&self) -> bool {
        // aim_pos varken de tick gerekli: boşta ortalama oradan çalışıyor.
        // aim_down_at ŞART: sıfırlamadan sonra aim_pos None, birikim boş
        // olabilir. Sayılmazsa çağıranın saat kolu kapanır, tick hiç
        // çağrılmaz ve iniş ASLA gerçekleşmez — nişan tamamen ölür.
        !self.swipes.is_empty() || self.aim_pending.is_some()
            || self.joystick_pending.is_some() || self.aim_pos.is_some()
            || self.aim_down_at.is_some()
            || self.aim_accum != (0.0, 0.0)
    }

    /// Devam eden jest sayısı (test ve teşhis için).
    pub fn swipe_count(&self) -> usize { self.swipes.len() }

    /// Motorun bildiği son zaman (test/teşhis).
    pub fn now_ms(&self) -> u64 { self.now_ms }

    /// Kullanımdaki işaretçi sayısı (teşhis).
    pub fn active_pointers(&self) -> usize { self.pool.active_count() }

    /// Nişan parmağının şu anki konumu (teşhis ve test).
    /// Sınırsız kipte ekran dışında olabilir.
    pub fn aim_position(&self) -> Option<Norm> { self.aim_pos }

    #[cfg(test)]
    fn forget_held_for_test(&mut self) { self.held.clear(); }

    /// Devir teslim için ikinci parmak indirilmiş mi (test/teşhis).
    pub fn aim_has_second(&self) -> bool { self.aim_second.is_some() }

    #[must_use = "üretilen dokunuş eylemleri arka uca GÖNDERİLMELİ"]
    pub fn handle(&mut self, ev: InputEvent) -> Vec<TouchAction> {
        if !self.enabled { return Vec::new(); }
        match ev {
            InputEvent::Press(t) => self.on_press(t),
            InputEvent::Release(t) => self.on_release(t),
            InputEvent::MouseMove { dx, dy } => {
                // Uygulama tick'te; burada yalnızca birikir.
                self.aim_accum.0 += dx;
                self.aim_accum.1 += dy;
                self.aim_last_move_ms = self.now_ms;
                Vec::new()
            }
        }
    }

    fn on_press(&mut self, t: TriggerKind) -> Vec<TouchAction> {
        // Tuş tekrarını (auto-repeat) yut: aksi halde Down olayları yığılır.
        if !self.held.insert(t) { return Vec::new(); }
        let Some((name, binding)) = self.owner(t) else { return Vec::new() };
        let (name, binding) = (name.to_string(), binding.clone());
        match binding {
            Binding::Tap { at, .. } | Binding::Toggle { at, .. } => {
                match self.pool.acquire(&name) {
                    Some(id) => vec![TouchAction::Down { id, at }],
                    None => {
                        // Sessizce yutmak "tuş bazen çalışmıyor" hatası
                        // üretir ve kullanıcı nedenini bulamaz.
                        tracing::warn!(bağlantı = %name, sınır = MAX_POINTERS,
                            "işaretçi havuzu dolu — bu dokunuş atlandı");
                        Vec::new()
                    }
                }
            }
            Binding::Joystick { .. } => self.recompute_joystick(&name),
            Binding::Aim { origin, .. } => {
                // Bekleyen gecikmeli iniş iptal: burada zaten iniyoruz,
                // ikisi birden çalışırsa ekranda iki nişan parmağı kalır.
                self.aim_down_at = None;
                self.aim_pos = Some(origin);
                match self.pool.acquire(&name) {
                    Some(id) => vec![TouchAction::Down { id, at: origin }],
                    None => Vec::new(),
                }
            }
            Binding::Swipe { from, to, duration_ms, ref group, easing, .. } => {
                // Aynı kaydırma zaten oynuyorsa yenisini başlatma.
                if self.swipes.iter().any(|s| s.binding == name) { return Vec::new(); }

                // Aynı gruptaki devam eden jestleri İPTAL ET: parmağı
                // bulunduğu yerden kaldır, jesti tamamlama. Kullanıcı fikrini
                // değiştirmiştir; yarım kalan kaydırma oyunda çoğu zaman
                // eşiğin altında kalır ve yanlış hareket üretmez.
                let mut acts = Vec::new();
                if let Some(g) = group {
                    let cancelled: Vec<(String, u8)> = self.swipes.iter()
                        .filter(|s| s.group.as_deref() == Some(g.as_str()))
                        .map(|s| (s.binding.clone(), s.id))
                        .collect();
                    for (b, id) in cancelled {
                        acts.push(TouchAction::Up { id });
                        self.pool.release(&b);
                        self.swipes.retain(|s| s.binding != b);
                    }
                }

                match self.pool.acquire(&name) {
                    Some(id) => {
                        self.swipes.push(ActiveSwipe {
                            binding: name.clone(), group: group.clone(), easing, id, from, to,
                            start_ms: self.now_ms,
                            duration_ms: duration_ms as u64,
                            last_step: 0,
                            last_sent: from,
                        });
                        acts.push(TouchAction::Down { id, at: from });
                        acts
                    }
                    None => acts,
                }
            }
        }
    }

    fn on_release(&mut self, t: TriggerKind) -> Vec<TouchAction> {
        if !self.held.remove(&t) { return Vec::new(); }
        let Some((name, binding)) = self.owner(t) else { return Vec::new() };
        let (name, binding) = (name.to_string(), binding.clone());
        match binding {
            Binding::Joystick { .. } => {
                // Hâlâ basılı yön varsa parmak kalkmaz, sadece yeniden konumlanır.
                let acts = self.recompute_joystick(&name);
                if acts.is_empty() {
                    self.pool.release(&name).map(|id| vec![TouchAction::Up { id }])
                        .unwrap_or_default()
                } else { acts }
            }
            Binding::Aim { .. } => {
                // Bekleyen iniş de iptal: nişan bırakıldıktan SONRA parmak
                // inerse oyunda sahipsiz bir dokunuş asılı kalır.
                self.aim_down_at = None;
                self.aim_pos = None;
                self.aim_accum = (0.0, 0.0);
                self.pool.release(&name).map(|id| vec![TouchAction::Up { id }])
                    .unwrap_or_default()
            }
            // Kaydırma ateşle-ve-unut: tuşun bırakılması jesti etkilemez.
            // Yarıda kesmek oyunlarda "yanlış kaydırma" olarak algılanır.
            Binding::Swipe { .. } => Vec::new(),
            _ => self.pool.release(&name).map(|id| vec![TouchAction::Up { id }])
                    .unwrap_or_default(),
        }
    }

    /// Basılı yönlere göre joystick parmağını konumlandırır.
    /// Hiç yön basılı değilse boş döner (çağıran parmağı kaldırır).
    fn recompute_joystick(&mut self, name: &str) -> Vec<TouchAction> {
        let Some(Binding::Joystick { up, down, left, right, center, radius }) =
            self.profile.bindings.get(name).cloned() else { return Vec::new() };
        let mut dx = 0.0f32;
        let mut dy = 0.0f32;
        if self.held.contains(&TriggerKind::from(&up))    { dy -= 1.0; }
        if self.held.contains(&TriggerKind::from(&down))  { dy += 1.0; }
        if self.held.contains(&TriggerKind::from(&left))  { dx -= 1.0; }
        if self.held.contains(&TriggerKind::from(&right)) { dx += 1.0; }
        if dx == 0.0 && dy == 0.0 { return Vec::new(); }

        // Köşegende hız artmasın diye normalize et — aksi halde çapraz
        // hareket %41 daha hızlı olur ve oyuncu bunu "kayıyor" diye hisseder.
        let len = (dx * dx + dy * dy).sqrt();
        // Dikey yarıçap en/boy oranıyla ölçeklenir; yoksa daire elips olur.
        let at = Norm::new(
            center.x + dx / len * radius,
            center.y + dy / len * radius * self.aspect,
        );

        let first = self.pool.get(name).is_none();
        match self.pool.acquire(name) {
            Some(id) if first => {
                // İlk basışta MERKEZE in; yön bir sonraki karede uygulanır.
                // Parmağı doğrudan kenara indirmek bazı oyunlarda hareketi
                // başlatmıyor — gerçek oyuncu ortaya basıp sürükler.
                self.joystick_pending = Some(name.to_string());
                vec![TouchAction::Down { id, at: center }]
            }
            Some(id) => vec![TouchAction::Move { id, at }],
            None => Vec::new(),
        }
    }

    fn on_mouse(&mut self, dx: f32, dy: f32) -> Vec<TouchAction> {
        let Some((name, Binding::Aim {
            toggle, origin, sensitivity, deadzone, recenter_margin, handoff,
            nonlinear, reset_delay_ms, unbounded, safety_span,
        })) = self.profile.bindings.iter()
                .find(|(_, b)| matches!(b, Binding::Aim { .. }))
                .map(|(n, b)| (n.clone(), b.clone()))
        else { return Vec::new() };

        if (dx * dx + dy * dy).sqrt() < deadzone { return Vec::new(); }

        // Sınırsız kip: aşağıdaki sıfırlama düzeneğinin TAMAMI atlanır.
        // Kenar payı, devir teslim, doğrusal olmayan ölçekleme ve gecikmeli
        // iniş hepsi tek bir kısıtı gizlemek için vardı — o kısıt bu yolda
        // yok (`docs/fare-nisan.md`).
        if unbounded && self.offscreen_ok {
            if toggle.is_some() && self.aim_pos.is_none() { return Vec::new(); }
            return self.on_mouse_free(&name, origin, sensitivity, safety_span, dx, dy);
        }

        // toggle yoksa nişan HER ZAMAN etkin: ilk fare hareketinde parmağı
        // indir. FPS'te bakış için tuşa basılı tutmak gerekmemeli.
        let mut acts = Vec::new();
        if self.aim_pos.is_none() {
            if toggle.is_some() { return acts; }
            let Some(id) = self.pool.acquire(&name) else {
                tracing::error!(
                    kullanımda = self.pool.active_count(), sınır = MAX_POINTERS,
                    "nişan işaretçi alamadı — havuz dolu, fare çalışmayacak");
                return acts;
            };
            self.aim_pos = Some(origin);
            acts.push(TouchAction::Down { id, at: origin });
        }
        let pos = self.aim_pos.expect("yukarıda kuruldu");
        let Some(id) = self.pool.get(&name) else { return acts };

        let m = recenter_margin.clamp(0.01, 0.45);
        let lo = m;
        let hi = 1.0 - m;
        let span = hi - lo;

        // Doğrusal olmayan ölçekleme: parmak merkezden uzaklaştıkça
        // hassasiyet düşer, kenara asimptotik yaklaşır ve pratikte hiç
        // varmaz. Yeniden yerleşme ihtiyacı BAŞTAN doğmaz.
        let scale = if nonlinear {
            let d = ((pos.x - origin.x).powi(2)
                   + ((pos.y - origin.y) / self.aspect.max(0.01)).powi(2)).sqrt();
            let min_d = span / 20.0;
            if d > min_d { (min_d / d).sqrt() } else { 1.0 }
        } else { 1.0 };

        let nx = pos.x + dx * sensitivity * scale;
        let ny = pos.y + dy * sensitivity * scale;

        if handoff {
            // --- Devir teslimli yol: hareket sırasında ASLA kaldırma ---
            //
            // İkinci parmak, birincisi kenara varmadan merkeze iner ve
            // ikisi birlikte hareket eder. Birincisi ancak kenara
            // dayandığında bırakılır; o an ikincisi zaten hareket
            // hâlindedir ve oyunun takibi kesilmez.
            const PREPARE: f32 = 0.62;

            let far = ((pos.x - origin.x).powi(2)
                     + (pos.y - origin.y).powi(2)).sqrt() > span * 0.5 * PREPARE;

            // Hazırlık: ikinci parmağı indir (henüz kimse kalkmıyor).
            if far && self.aim_second.is_none() {
                // Merkez DEĞİL: hareket yönünün tersindeki en uzak nokta.
                // Merkeze koymak kalan yolun yarısını verir; ters uca
                // koymak tamamını verir ve devir sıklığını yarıya indirir.
                let seat = self.reseat_point(origin, m, dx, dy);
                if let Some(id2) = self.pool.acquire(&second_key(&name)) {
                    acts.push(TouchAction::Down { id: id2, at: seat });
                    self.aim_second = Some(seat);
                }
            }

            let out = nx < lo || nx > hi || ny < lo || ny > hi;

            // İkinci parmak varsa onu da AYNI delta ile taşı.
            if let Some(sp) = self.aim_second {
                let s_next = Norm::new(
                    (sp.x + dx * sensitivity).clamp(lo, hi),
                    (sp.y + dy * sensitivity).clamp(lo, hi),
                );
                if let Some(id2) = self.pool.get(&second_key(&name)) {
                    acts.push(TouchAction::Move { id: id2, at: s_next });
                }
                self.aim_second = Some(s_next);
            }

            if out {
                // Devir: birinciyi bırak, ikinci artık birincidir.
                // İkinci bu karede zaten hareket etti — boşluk yok.
                if let Some(sp) = self.aim_second.take() {
                    acts.push(TouchAction::Up { id });
                    self.pool.release(&name);
                    self.pool.rename(&second_key(&name), &name);
                    self.aim_pos = Some(sp);
                } else {
                    // İkinci hazır değil (ani sıçrama): basit yola düş.
                    if let Some((old_id, new_id)) = self.pool.rotate(&name) {
                        acts.push(TouchAction::Up { id: old_id });
                        acts.push(TouchAction::Down { id: new_id, at: origin });
                        self.aim_pos = Some(origin);
                        let (px, py) = self.aim_pending.unwrap_or((0.0, 0.0));
                        self.aim_pending = Some((px + dx, py + dy));
                    }
                }
                return acts;
            }

            let next = Norm::new(nx, ny);
            self.aim_pos = Some(next);
            acts.push(TouchAction::Move { id, at: next });
            return acts;
        }

        if nx < lo || nx > hi || ny < lo || ny > hi {
            // Basit yol: farklı slot şart, aksi halde kalkış ve iniş tek
            // SYN_REPORT içinde birleşir ve oyun ışınlanma görür.
            // Önce UYGULANABİLİR kısmı gönder: parmağı sınıra taşı.
            // Atmak dönüşü kaybettirir; sınıra kadar olan kısım geçerli.
            let edge = Norm::new(nx.clamp(lo, hi), ny.clamp(lo, hi));
            if (edge.x - pos.x).abs() > f32::EPSILON
                || (edge.y - pos.y).abs() > f32::EPSILON
            {
                acts.push(TouchAction::Move { id, at: edge });
            }

            // Kalkış ŞİMDİ, iniş GECİKMELİ.
            //
            // Aynı karede göndermek Android'in kalkışı gerçek bir dokunuş
            // sonu olarak işlemesine fırsat vermiyor; oyun ışınlanma
            // görüyor. XtMapper de araya gecikme koyuyor.
            acts.push(TouchAction::Up { id });
            self.pool.release(&name);
            self.aim_pos = None;
            self.aim_down_at = Some(self.now_ms + reset_delay_ms.max(1) as u64);

            // TAŞAN kısım ATILIR, birikime EKLENMEZ.
            //
            // Eklenirse iniş anında sınır yeniden aşılır ve anında yeni bir
            // sıfırlama tetiklenir — döngüye girer ve kullanıcı "hareketimi
            // hiç algılamıyor" der. Taşan miktar zaten dokunuş alanında
            // temsil edilemez.
            return acts;
        }

        let next = Norm::new(nx, ny);
        self.aim_pos = Some(next);
        acts.push(TouchAction::Move { id, at: next });
        acts
    }

    /// Sınırsız nişan: parmak bir kez iner, sonra sadece hareket eder.
    ///
    /// Kalkış yok, ortalama yok, devir teslim yok. Üç belirtinin de
    /// (hiç algılamama, saniyelerce ölü bölge, aim kayması) kaynağı
    /// bunlardı; ortadan kaldırınca hafifletmeye de gerek kalmıyor.
    ///
    /// Hassasiyet SABİT: `nonlinear` bilerek uygulanmıyor. Parmağın
    /// görünmeyen konumuna göre değişen hassasiyet, FPS'te kas hafızasını
    /// imkânsız kılıyor ve kullanıcı bunu "aim kayıyor" diye yaşıyordu.
    fn on_mouse_free(
        &mut self, name: &str, origin: Norm, sensitivity: f32,
        safety_span: f32, dx: f32, dy: f32,
    ) -> Vec<TouchAction> {
        let mut acts = Vec::new();

        if self.aim_pos.is_none() {
            // Emniyet sıfırlaması sürüyorsa karışma: iki nişan parmağı olur.
            if self.aim_down_at.is_some() { return acts; }
            let Some(id) = self.pool.acquire(name) else {
                tracing::error!(
                    kullanımda = self.pool.active_count(), sınır = MAX_POINTERS,
                    "nişan işaretçi alamadı — havuz dolu, fare çalışmayacak");
                return acts;
            };
            self.aim_pos = Some(origin);
            acts.push(TouchAction::Down { id, at: origin });
            // İniş KENDİ karesinde yalnız kalmalı; hareket bir sonrakine
            // ertelenir. Aynı SYN_REPORT'ta gitseydi oyun dokunuşu
            // doğrudan hedefte görür ve delta hesaplayamazdı.
            let (px, py) = self.aim_pending.unwrap_or((0.0, 0.0));
            self.aim_pending = Some((px + dx, py + dy));
            return acts;
        }

        let (Some(pos), Some(id)) = (self.aim_pos, self.pool.get(name))
        else { return acts };
        let next = free_step(pos, origin, safety_span, dx * sensitivity, dy * sensitivity);
        self.aim_pos = Some(next);
        acts.push(TouchAction::Move { id, at: next });
        acts
    }

    /// Fare durduysa ve parmak merkezden uzaklaşmışsa sessizce ortalar.
    ///
    /// Boştayken merkeze dönmek doğru: bir sonraki hareketin yönü
    /// bilinmiyor, merkez her yöne eşit yol bırakır.
    fn idle_recenter(&mut self) -> Vec<TouchAction> {
        // Gerçek oyunda mikro duraklamalar çok: nişan alma, köşe dönme,
        // ateş etme aralarında 30-40 ms boşluklar oluyor. Eşiği düşürmek
        // sıfırlamaların çoğunu bu boşluklara kaydırıyor ve hareket
        // sırasındaki sıfırlama ihtiyacını azaltıyor.
        const IDLE_MS: u64 = 35;
        /// Merkezden bu oranda uzaklaşınca sıfırlamaya değer.
        const FAR: f32 = 0.15;

        let Some((name, Binding::Aim {
            origin, recenter_margin, reset_delay_ms, unbounded, safety_span, ..
        })) = self.profile.bindings.iter()
                .find(|(_, b)| matches!(b, Binding::Aim { .. }))
                .map(|(n, b)| (n.clone(), b.clone()))
        else { return Vec::new() };
        let Some(pos) = self.aim_pos else { return Vec::new() };
        if self.now_ms.saturating_sub(self.aim_last_move_ms) < IDLE_MS {
            return Vec::new();
        }
        let d = ((pos.x - origin.x).powi(2) + (pos.y - origin.y).powi(2)).sqrt();
        let threshold = if unbounded && self.offscreen_ok {
            // Sınırsız kipte ortalamanın TEK gerekçesi sayısal aralık.
            // Eşik emniyet kutusunun yarısı: normal oyunda hiç
            // tetiklenmez, tetiklendiğinde de fare zaten durmuştur.
            safety_span.max(1.0) * 0.5
        } else {
            let m = recenter_margin.clamp(0.01, 0.45);
            (1.0 - 2.0 * m) * FAR
        };
        if d < threshold { return Vec::new(); }

        // Kalkış ŞİMDİ, iniş GECİKMELİ ve KENDİ karesinde.
        //
        // Eskiden ikisi tek dizide dönüyordu; arka uç bunu tek
        // SYN_REPORT'a çevirdiği için Android dokunuşun bittiğini
        // göremiyor ve oyun ışınlanma görüyordu — kodun başka yerinde
        // defalarca uyarılan hatanın ta kendisi.
        let Some(id) = self.pool.release(&name) else { return Vec::new() };
        self.aim_pos = None;
        // Birikmiş hareketi at: fare zaten duruyor, taşıyacağı bilgi yok
        // ama inişten sonra sahte bir sıçrama üretebilir.
        self.aim_accum = (0.0, 0.0);
        self.aim_last_move_ms = self.now_ms;
        self.aim_down_at = Some(self.now_ms + reset_delay_ms.max(1) as u64);
        vec![TouchAction::Up { id }]
    }

    /// Yeniden yerleştirme noktasını HAREKET YÖNÜNE göre seçer.
    ///
    /// Merkeze dönmek yolun yarısını harcar. Oyuncu sağa dönüyorsa parmağı
    /// SOL kenara koymak tüm genişliği kullandırır ve ortalama sıklığını
    /// yaklaşık yarıya indirir.
    ///
    /// Bu önemli çünkü her ortalamanın iki bedeli var ve ikisi de
    /// kaldırılamaz: (1) yeni dokunuşun önceki konumu olmadığı için bir
    /// karelik delta boşluğu, (2) Android'in dokunuş toleransı — uygulama
    /// sürükleme saymadan önce belli bir mesafe yutar. Tek çare bu
    /// olayların SAYISINI azaltmak.
    fn reseat_point(&self, origin: Norm, m: f32, dx: f32, dy: f32) -> Norm {
        let len = (dx * dx + dy * dy).sqrt();
        if len < f32::EPSILON { return origin; }
        let (ux, uy) = (dx / len, dy / len);
        let lo = m;
        let hi = 1.0 - m;
        // Hareket yönünün TERSİNE gidebildiğimiz kadar git.
        let t_axis = |p: f32, u: f32| -> f32 {
            if u.abs() < 1e-6 { f32::INFINITY }
            else if u > 0.0 { (p - lo) / u }   // ters yön: azalan
            else { (hi - p) / -u }
        };
        let t = t_axis(origin.x, ux).min(t_axis(origin.y, uy)).max(0.0);
        if !t.is_finite() { return origin; }

        // TAM kenara oturma: ters yönde hiç pay kalmaz ve en ufak geri
        // hareket anında yeni bir yerleşme tetikler. Kullanıcı bunu
        // "hareketlerimi algılamıyor" diye yaşıyor — arka arkaya yerleşme
        // her biri bir kare yutuyor.
        //
        // %80'ini kullan: kalan yolun büyük kısmını korurken geriye de
        // yaklaşık %20'lik bir tampon bırak.
        const USE: f32 = 0.80;
        Norm::new(origin.x - ux * t * USE, origin.y - uy * t * USE)
    }

    /// Ertelenmiş fare hareketini uygular (iniş sonrası ilk kare).
    fn apply_aim_delta(&mut self, dx: f32, dy: f32) -> Vec<TouchAction> {
        let Some((name, Binding::Aim {
            origin, sensitivity, recenter_margin, unbounded, safety_span, ..
        })) = self.profile.bindings.iter()
                .find(|(_, b)| matches!(b, Binding::Aim { .. }))
                .map(|(n, b)| (n.clone(), b.clone()))
        else { return Vec::new() };
        let (Some(pos), Some(id)) = (self.aim_pos, self.pool.get(&name))
        else { return Vec::new() };
        let next = if unbounded && self.offscreen_ok {
            free_step(pos, origin, safety_span, dx * sensitivity, dy * sensitivity)
        } else {
            let m = recenter_margin.clamp(0.01, 0.45);
            Norm::new(
                (pos.x + dx * sensitivity).clamp(m, 1.0 - m),
                (pos.y + dy * sensitivity).clamp(m, 1.0 - m),
            )
        };
        self.aim_pos = Some(next);
        vec![TouchAction::Move { id, at: next }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::profile::Binding;
    use std::collections::BTreeMap;

    const W: u16 = 17; const A: u16 = 30; const S: u16 = 31; const D: u16 = 32;
    const SPACE: u16 = 57;

    fn joystick_profile() -> Profile {
        let mut b = BTreeMap::new();
        b.insert("hareket".into(), Binding::Joystick {
            up: Trigger::Key(W), down: Trigger::Key(S),
            left: Trigger::Key(A), right: Trigger::Key(D),
            center: Norm::new(0.2, 0.7), radius: 0.1,
        });
        b.insert("zipla".into(), Binding::Tap {
            trigger: Trigger::Key(SPACE), at: Norm::new(0.9, 0.8),
        });
        Profile { name: "t".into(), package: "p".into(), bindings: b }
    }

    fn aim_profile() -> Profile {
        let mut b = BTreeMap::new();
        b.insert("nisan".into(), Binding::Aim {
            toggle: Some(Trigger::MouseRight),
            origin: Norm::new(0.5, 0.5),
            sensitivity: 0.001,
            deadzone: 0.5,
            recenter_margin: 0.12,
            handoff: false, nonlinear: false, reset_delay_ms: 0,
            // Bu testler SINIRLI yolu sınıyor.
            unbounded: false, safety_span: 32.0,
        });
        Profile { name: "t".into(), package: "p".into(), bindings: b }
    }

    fn key(k: u16) -> TriggerKind { TriggerKind::Key(k) }

    /// Fare hareketi artık kare başına uygulanıyor: olay biriktirir,
    /// tick uygular. Testler gerçek akışı taklit etmeli.
    fn mouse(e: &mut Engine, dx: f32, dy: f32) -> Vec<TouchAction> {
        let _ = e.handle(InputEvent::MouseMove { dx, dy });
        e.tick(e.now_ms() + 1)
    }

    #[test]
    fn tap_presses_and_lifts() {
        let mut e = Engine::new(joystick_profile());
        let down = e.handle(InputEvent::Press(key(SPACE)));
        assert!(matches!(down[..], [TouchAction::Down { .. }]));
        let up = e.handle(InputEvent::Release(key(SPACE)));
        assert!(matches!(up[..], [TouchAction::Up { .. }]));
    }

    /// Klavye auto-repeat Down olaylarını yığmamalı.
    #[test]
    fn key_autorepeat_is_swallowed() {
        let mut e = Engine::new(joystick_profile());
        assert_eq!(e.handle(InputEvent::Press(key(SPACE))).len(), 1);
        assert!(e.handle(InputEvent::Press(key(SPACE))).is_empty(), "tekrar Down üretmemeli");
        assert!(e.handle(InputEvent::Press(key(SPACE))).is_empty());
    }

    /// İlk basış MERKEZE iner; yön bir sonraki karede uygulanır.
    /// Parmağı doğrudan kenara indirmek bazı oyunlarda hareketi başlatmıyor.
    #[test]
    fn joystick_first_press_lands_on_center() {
        let mut e = Engine::new(joystick_profile());
        let a = e.handle(InputEvent::Press(key(W)));
        match a[..] {
            [TouchAction::Down { at, .. }] => {
                assert!((at.x - 0.2).abs() < 1e-5, "merkez x: {at:?}");
                assert!((at.y - 0.7).abs() < 1e-5, "merkez y: {at:?}");
            }
            _ => panic!("Down bekleniyordu: {a:?}"),
        }
        assert!(e.has_pending(), "yön bir sonraki kareye ertelenmeli");
    }

    #[test]
    fn joystick_direction_applied_on_next_tick() {
        let mut e = Engine::new(joystick_profile());
        e.handle(InputEvent::Press(key(W)));
        let a = e.tick(5);
        match a[..] {
            [TouchAction::Move { at, .. }] =>
                assert!(at.y < 0.7, "yukarı gitmeli: {at:?}"),
            _ => panic!("Move bekleniyordu: {a:?}"),
        }
    }

    /// Yarıçap PİKSEL uzayında daire olmalı. Normalize koordinat eksen
    /// başına ölçekli olduğu için dikey bileşen en/boy oranıyla çarpılır;
    /// yoksa 2560x1440'ta A/D, W/S'den 1.78 kat uzağa gider.
    #[test]
    fn joystick_is_circular_in_pixels_not_normalised_units() {
        let mut e = Engine::new(joystick_profile());
        e.set_aspect(2560, 1440);
        let (w, h) = (2560.0f32, 1440.0f32);

        e.handle(InputEvent::Press(key(W)));
        let up = match e.tick(5)[..] { [TouchAction::Move { at, .. }] => at, _ => panic!() };
        let dy_px = (0.7 - up.y) * h;

        let mut e2 = Engine::new(joystick_profile());
        e2.set_aspect(2560, 1440);
        e2.handle(InputEvent::Press(key(D)));
        let right = match e2.tick(5)[..] { [TouchAction::Move { at, .. }] => at, _ => panic!() };
        let dx_px = (right.x - 0.2) * w;

        assert!((dx_px - dy_px).abs() < 2.0,
            "piksel mesafeleri eşit olmalı: yatay {dx_px:.1}px, dikey {dy_px:.1}px");
    }

    #[test]
    fn joystick_second_direction_moves_not_redowns() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
        let a = e.handle(InputEvent::Press(key(D)));
        assert!(matches!(a[..], [TouchAction::Move { .. }]), "ikinci yön Move olmalı: {a:?}");
    }

    /// Köşegen hareket hızlanmamalı — normalize edilmiş olmalı.
    #[test]
    fn diagonal_is_normalised_not_faster() {
        let mut e = Engine::new(joystick_profile());
        e.set_aspect(1000, 1000);   // kare ekran: en-boy düzeltmesi 1
        let _ = e.handle(InputEvent::Press(key(W)));
        let _ = e.tick(5);          // ilk basış merkeze iner, yön sonra
        let a = e.handle(InputEvent::Press(key(D)));
        let at = match a[..] { [TouchAction::Move { at, .. }] => at, _ => panic!() };
        let dist = ((at.x - 0.2).powi(2) + (at.y - 0.7).powi(2)).sqrt();
        assert!((dist - 0.1).abs() < 1e-4,
                "çapraz mesafe yarıçapa eşit olmalı, {dist} bulundu");
    }

    /// Bir yön bırakılınca diğeri basılıysa parmak KALKMAMALI.
    #[test]
    fn releasing_one_direction_keeps_finger_down() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
        let _ = e.handle(InputEvent::Press(key(D)));
        let a = e.handle(InputEvent::Release(key(W)));
        assert!(matches!(a[..], [TouchAction::Move { .. }]),
                "hâlâ D basılı, parmak kalkmamalı: {a:?}");
    }

    #[test]
    fn releasing_last_direction_lifts_finger() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
        let a = e.handle(InputEvent::Release(key(W)));
        assert!(matches!(a[..], [TouchAction::Up { .. }]), "{a:?}");
    }

    #[test]
    fn joystick_and_tap_use_separate_pointers() {
        let mut e = Engine::new(joystick_profile());
        let j = e.handle(InputEvent::Press(key(W)));
        let t = e.handle(InputEvent::Press(key(SPACE)));
        let jid = match j[..] { [TouchAction::Down { id, .. }] => id, _ => panic!() };
        let tid = match t[..] { [TouchAction::Down { id, .. }] => id, _ => panic!() };
        assert_ne!(jid, tid, "eşzamanlı bağlantılar ayrı işaretçi almalı");
    }

    #[test]
    fn unmapped_key_produces_nothing() {
        let mut e = Engine::new(joystick_profile());
        assert!(e.handle(InputEvent::Press(key(99))).is_empty());
    }

    #[test]
    fn aim_moves_finger_from_origin() {
        let mut e = Engine::new(aim_profile());
        let _ = e.handle(InputEvent::Press(TriggerKind::MouseRight));
        let a = mouse(&mut e, 100.0, 0.0);
        match a[..] {
            [TouchAction::Move { at, .. }] =>
                assert!((at.x - 0.6).abs() < 1e-5, "0.5 + 100*0.001 = 0.6, {at:?}"),
            _ => panic!("Move bekleniyordu: {a:?}"),
        }
    }

    /// Deadzone altındaki gürültü olay üretmemeli.
    #[test]
    fn aim_ignores_jitter_below_deadzone() {
        let mut e = Engine::new(aim_profile());
        let _ = e.handle(InputEvent::Press(TriggerKind::MouseRight));
        assert!(mouse(&mut e, 0.2, 0.1).is_empty());
    }

    /// toggle TANIMLIYKEN, basılmadan fare hareketi dokunuş üretmemeli.
    #[test]
    fn mouse_move_without_aim_active_does_nothing() {
        let mut e = Engine::new(aim_profile());
        assert!(mouse(&mut e, 100.0, 0.0).is_empty());
    }

    /// toggle YOKSA nişan her zaman etkin: ilk fare hareketi parmağı indirir.
    /// FPS'te bakış için tuşa basılı tutmak gerekmemeli.
    fn aim_engine(handoff: bool) -> Engine {
        let mut b = BTreeMap::new();
        b.insert("bakis".into(), Binding::Aim {
            toggle: None,
            origin: Norm::new(0.5, 0.5),
            sensitivity: 0.001,
            deadzone: 0.5,
            recenter_margin: 0.12,
            handoff, nonlinear: false, reset_delay_ms: 0,
            unbounded: false, safety_span: 32.0,
        });
        Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b })
    }
    fn always_on_aim() -> Engine { aim_engine(false) }

    /// DEVİR TESLİM: hareket sırasında ASLA parmak kaldırılmamalı.
    ///
    /// Kullanıcı bunu "köşeye gelince duruyor, sonra devam ediyor" diye
    /// yaşadı: kaldır-koy yolunda devir anında dönüş kesiliyor ve oyunun
    /// dokunuş yumuşatması sıfırlanıyor.
    #[test]
    fn handoff_never_lifts_without_a_moving_finger() {
        let mut e = aim_engine(true);
        let mut lifted_alone = 0usize;
        for _ in 0..60 {
            let acts = mouse(&mut e, 120.0, 0.0);
            let has_up = acts.iter().any(|a| matches!(a, TouchAction::Up { .. }));
            let has_move = acts.iter().any(|a| matches!(a, TouchAction::Move { .. }));
            if has_up && !has_move { lifted_alone += 1; }
            let _ = e.tick(0);
        }
        assert_eq!(lifted_alone, 0,
            "her kaldırma karesinde hareket eden bir parmak olmalı");
    }

    /// Devirden önce ikinci parmak İNDİRİLMELİ.
    #[test]
    fn handoff_places_second_finger_before_the_edge() {
        let mut e = aim_engine(true);
        let mut saw_prepare = false;
        for _ in 0..40 {
            let acts = mouse(&mut e, 120.0, 0.0);
            // Kaldırma OLMADAN inen bir parmak = hazırlık.
            if acts.iter().any(|a| matches!(a, TouchAction::Down { .. }))
                && !acts.iter().any(|a| matches!(a, TouchAction::Up { .. }))
                && saw_prepare == false
            {
                // İlk Down başlangıç dokunuşu olabilir; ikincisini ara.
                saw_prepare = e.aim_has_second();
            }
            if saw_prepare { break; }
        }
        assert!(saw_prepare, "kenara varmadan ikinci parmak inmeli");
    }

    /// Devir sonrası dönüş kesintisiz sürmeli.
    #[test]
    fn handoff_keeps_rotating_across_the_edge() {
        let mut e = aim_engine(true);
        let mut frames_without_move = 0usize;
        for _ in 0..80 {
            let acts = mouse(&mut e, 150.0, 0.0);
            if !acts.iter().any(|a| matches!(a, TouchAction::Move { .. })) {
                frames_without_move += 1;
            }
            let _ = e.tick(0);
        }
        // İlk kare (Down) hariç her karede hareket olmalı.
        assert!(frames_without_move <= 1,
            "{frames_without_move} karede dönüş kesildi");
    }

    /// İlk hareket parmağı indirir VE aynı çağrıda hareket ettirir:
    /// bir kare beklemek gereksiz gecikme olurdu.
    #[test]
    fn aim_without_toggle_activates_on_first_motion() {
        let mut e = always_on_aim();
        let a = mouse(&mut e, 50.0, 0.0);
        match a[..] {
            [TouchAction::Down { at: d, .. }, TouchAction::Move { at: m, .. }] => {
                assert!((d.x - 0.5).abs() < 1e-5, "merkezde başlamalı: {d:?}");
                assert!((m.x - 0.55).abs() < 1e-5, "0.5 + 50*0.001: {m:?}");
            }
            _ => panic!("Down+Move bekleniyordu: {a:?}"),
        }
    }

    /// Kenara gelince parmak KALDIRILIP merkeze konmalı; yoksa sınırlı
    /// açıdan fazla dönülemez.
    #[test]
    fn aim_recenters_at_edge() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);   // -> 0.51
        // 0.51 + 400*0.001 = 0.91 > 1 - 0.12 = 0.88  -> yeniden ortala
        let a = mouse(&mut e, 400.0, 0.0);
        // Uygulanabilir kısım + kalkış; iniş gecikmeli (ayrı SYN_REPORT şart).
        assert!(a.iter().any(|x| matches!(x, TouchAction::Up { .. })),
            "kalkış olmalı: {a:?}");
        assert!(!a.iter().any(|x| matches!(x, TouchAction::Down { .. })),
            "iniş aynı karede OLMAMALI: {a:?}");
        // Gecikme dolunca iniş gelir.
        let b = e.tick(e.now_ms() + 20);
        assert!(b.iter().any(|x| matches!(x, TouchAction::Down { .. })),
            "gecikme sonrası iniş gelmeli: {b:?}");
    }

    /// Yeniden yerleşme boşluğu TEK tick olmalı.
    ///
    /// Ertelenen ile o sırada biriken ayrı uygulanırsa boşluk iki tick'e
    /// çıkıyor ve kullanıcı bunu "kenarda hareketlerimi algılamıyor" diye
    /// yaşıyor.
    #[test]
    fn recenter_gap_is_a_single_tick() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);
        // Kenara dayanacak kadar büyük hareket -> yeniden yerleşme.
        let a = mouse(&mut e, 400.0, 0.0);
        assert!(!a.iter().any(|x| matches!(x, TouchAction::Down { .. })),
            "iniş aynı karede olmamalı: {a:?}");
        let b = e.tick(e.now_ms() + 20);
        assert!(b.iter().any(|x| matches!(x, TouchAction::Down { .. })),
            "gecikme sonrası iniş: {b:?}");
    }

    /// Yeniden ortalama HAREKET KAYBETMEMELİ.
    ///
    /// Kaybederse her ortalamada bir miktar dönüş yok olur ve fare ile
    /// görüş birbirinden ayrılır — kullanıcı bunu "aimde kayma" diye yaşar.
    #[test]
    fn recentering_preserves_the_frames_motion() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);   // -> 0.51
        // Sınırı aşan karede UYGULANABİLİR kısım gönderilmeli — atılmamalı.
        let a = mouse(&mut e, 400.0, 0.0);
        let moved = a.iter().find_map(|x| match x {
            TouchAction::Move { at, .. } => Some(*at), _ => None,
        }).expect("sınıra kadar olan hareket uygulanmalı");
        assert!(moved.x > 0.5, "ileri gitmeli: {moved:?}");
    }

    /// Uzun süreli çevirmede hareket KAYBOLMAMALI.
    ///
    /// Gecikmeli sıfırlama sırasında hareket birikir; iniş sonrası
    /// uygulanır. Toplam yol, gönderilen fare mesafesiyle orantılı kalmalı.
    #[test]
    fn long_turn_loses_no_motion() {
        let mut e = always_on_aim();
        let mut moves = 0usize;
        let mut downs = 0usize;
        let mut t = 0u64;
        for _ in 0..60 {
            for act in mouse(&mut e, 200.0, 0.0) {
                match act {
                    TouchAction::Move { .. } => moves += 1,
                    TouchAction::Down { .. } => downs += 1,
                    _ => {}
                }
            }
            t = e.now_ms() + 20;
            for act in e.tick(t) {
                match act {
                    TouchAction::Move { .. } => moves += 1,
                    TouchAction::Down { .. } => downs += 1,
                    _ => {}
                }
            }
        }
        assert!(downs >= 1, "en az bir sıfırlama olmalı");
        assert!(moves > 40, "hareketlerin çoğu uygulanmalı, {moves} bulundu");
    }

    /// Yeniden ortalama sonrası dönüş DEVAM etmeli.
    #[test]
    fn aim_continues_after_recenter() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);
        let _ = mouse(&mut e, 400.0, 0.0);       // kalkış
        let _ = e.tick(e.now_ms() + 20);         // gecikmeli iniş
        // İniş sonrası hareket normal Move üretmeli.
        let a = mouse(&mut e, 100.0, 0.0);
        assert!(a.iter().any(|x| matches!(x, TouchAction::Move { .. })),
            "iniş sonrası hareket Move üretmeli: {a:?}");
    }

    /// Dikey kenar da yeniden ortalamalı.
    #[test]
    fn aim_recenters_on_vertical_edge() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 0.0, 10.0);   // -> 0.51
        // 0.51 - 450*0.001 = 0.06 < 0.12  -> yeniden ortala
        let a = mouse(&mut e, 0.0, -450.0);
        assert!(a.iter().any(|x| matches!(x, TouchAction::Up { .. })), "{a:?}");
        assert!(!a.iter().any(|x| matches!(x, TouchAction::Down { .. })), "{a:?}");
        let b = e.tick(e.now_ms() + 20);
        assert!(b.iter().any(|x| matches!(x, TouchAction::Down { .. })), "{b:?}");
    }

    /// Yeniden ortalama işaretçi kimliğini tüketmemeli.
    #[test]
    fn recentering_does_not_leak_pointers() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);
        for _ in 0..50 {
            let _ = mouse(&mut e, 400.0, 0.0);
        }
        // Hâlâ çalışıyor olmalı: havuz tükenmiş olsaydı boş dönerdi.
        let a = mouse(&mut e, 20.0, 0.0);
        assert!(!a.is_empty(), "havuz sızdırmış olabilir");
    }

    /// Motor kapatılınca takılı parmak kalmamalı.
    #[test]
    fn disabling_lifts_every_held_finger() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(W)));
        let _ = e.handle(InputEvent::Press(key(SPACE)));
        let acts = e.set_enabled(false);
        assert_eq!(acts.len(), 2, "iki parmak da kalkmalı: {acts:?}");
        assert!(acts.iter().all(|a| matches!(a, TouchAction::Up { .. })));
    }

    #[test]
    fn disabled_engine_produces_nothing() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.set_enabled(false);
        assert!(e.handle(InputEvent::Press(key(SPACE))).is_empty());
    }

    /// Basılmamış tuşun bırakılması olay üretmemeli (odak kaybı sonrası).
    #[test]
    fn release_without_press_is_ignored() {
        let mut e = Engine::new(joystick_profile());
        assert!(e.handle(InputEvent::Release(key(SPACE))).is_empty());
    }

    fn swipe_engine() -> Engine {
        let mut b = BTreeMap::new();
        b.insert("sol".into(), Binding::Swipe {
            trigger: Trigger::Key(A),
            from: Norm::new(0.5, 0.5), to: Norm::new(0.2, 0.5), duration_ms: 80,
            group: None, easing: Easing::Linear,
        });
        Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b })
    }

    /// Aynı grupta iki kaydırma: ikincisi birincisini İPTAL etmeli.
    /// Gerçek oyun geri bildirimi: A'ya sonra hızlıca W'ye basınca
    /// oyun iki ayrı parmak görüyordu ve hareketler karışıyordu.
    fn grouped_engine() -> Engine {
        let mut b = BTreeMap::new();
        b.insert("sol".into(), Binding::Swipe {
            trigger: Trigger::Key(A),
            from: Norm::new(0.5, 0.5), to: Norm::new(0.2, 0.5), duration_ms: 80,
            group: Some("hareket".into()), easing: Easing::Linear,
        });
        b.insert("zipla".into(), Binding::Swipe {
            trigger: Trigger::Key(W),
            from: Norm::new(0.5, 0.6), to: Norm::new(0.5, 0.3), duration_ms: 80,
            group: Some("hareket".into()), easing: Easing::Linear,
        });
        b.insert("ates".into(), Binding::Swipe {
            trigger: Trigger::Key(SPACE),
            from: Norm::new(0.9, 0.8), to: Norm::new(0.9, 0.7), duration_ms: 80,
            group: None, easing: Easing::Linear,
        });
        Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b })
    }

    #[test]
    fn second_gesture_in_group_cancels_the_first() {
        let mut e = grouped_engine();
        let a = e.handle(InputEvent::Press(key(A)));
        let first_id = match a[..] { [TouchAction::Down { id, .. }] => id, _ => panic!("{a:?}") };
        let _ = e.tick(20);
        let w = e.handle(InputEvent::Press(key(W)));
        // Önce iptal (Up), sonra yeni jest (Down).
        match w[..] {
            [TouchAction::Up { id }, TouchAction::Down { .. }] =>
                assert_eq!(id, first_id, "iptal edilen parmak ilki olmalı"),
            _ => panic!("Up sonra Down bekleniyordu: {w:?}"),
        }
        assert_eq!(e.swipe_count(), 1, "yalnızca yeni jest kalmalı");
    }

    /// İptal edilen jest artık ilerlememeli.
    #[test]
    fn cancelled_gesture_stops_ticking() {
        let mut e = grouped_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        let _ = e.tick(20);
        let _ = e.handle(InputEvent::Press(key(W)));
        // Sonraki tick'ler yalnızca W'nin jestini ilerletmeli: dikey hareket.
        let mut moves = Vec::new();
        for ms in (25..=110).step_by(5) {
            for act in e.tick(ms) {
                if let TouchAction::Move { at, .. } = act { moves.push(at); }
            }
        }
        assert!(!moves.is_empty());
        assert!(moves.iter().all(|m| (m.x - 0.5).abs() < 1e-4),
                "yalnızca dikey hareket olmalı, yatay iz kalmamalı");
    }

    /// Grupsuz jest gruptakilerden ETKİLENMEMELİ — nişancı oyunlarında
    /// joystick + nişan + ateş eşzamanlı olmalı.
    #[test]
    fn ungrouped_gesture_is_not_cancelled() {
        let mut e = grouped_engine();
        let _ = e.handle(InputEvent::Press(key(SPACE)));
        let _ = e.tick(10);
        let a = e.handle(InputEvent::Press(key(A)));
        assert!(matches!(a[..], [TouchAction::Down { .. }]),
                "grupsuz jest iptal edilmemeli: {a:?}");
        assert_eq!(e.swipe_count(), 2, "iki jest birlikte sürmeli");
    }

    #[test]
    fn swipe_starts_with_finger_down_at_origin() {
        let mut e = swipe_engine();
        let a = e.handle(InputEvent::Press(key(A)));
        match a[..] {
            [TouchAction::Down { at, .. }] => assert!((at.x - 0.5).abs() < 1e-5),
            _ => panic!("{a:?}"),
        }
        assert!(e.has_pending(), "jest devam etmeli");
    }

    /// Kaydırma ARA ADIMLAR üretmeli — tek sıçrama oyunlarda jest sayılmaz.
    #[test]
    fn swipe_emits_intermediate_steps() {
        let mut e = swipe_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        let mut moves = 0;
        for ms in (0..=80).step_by(5) {
            for act in e.tick(ms) {
                if matches!(act, TouchAction::Move { .. }) { moves += 1; }
            }
        }
        assert!(moves >= 8, "en az 8 ara adım bekleniyordu, {moves} üretildi");
    }

    #[test]
    fn swipe_reaches_target_and_lifts() {
        let mut e = swipe_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        let mut last_pos = None;
        let mut lifted = false;
        for ms in (0..=100).step_by(5) {
            for act in e.tick(ms) {
                match act {
                    TouchAction::Move { at, .. } => last_pos = Some(at),
                    TouchAction::Up { .. } => lifted = true,
                    _ => {}
                }
            }
        }
        assert!(lifted, "jest sonunda parmak kalkmalı");
        let at = last_pos.expect("hiç hareket üretilmedi");
        assert!((at.x - 0.2).abs() < 1e-4, "hedefe ulaşmalı, {at:?}");
        assert!(!e.has_pending(), "biten jest listede kalmamalı");
    }

    /// Ateşle-ve-unut: tuşu bırakmak jesti kesmemeli.
    #[test]
    fn releasing_key_does_not_abort_swipe() {
        let mut e = swipe_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        let _ = e.tick(20);
        let a = e.handle(InputEvent::Release(key(A)));
        assert!(a.is_empty(), "bırakma olay üretmemeli: {a:?}");
        assert!(e.has_pending(), "jest devam etmeli");
    }

    /// Aynı kaydırma oynarken tekrar basmak ikinci jest başlatmamalı.
    #[test]
    fn repeated_press_does_not_stack_swipes() {
        let mut e = swipe_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        let _ = e.handle(InputEvent::Release(key(A)));
        let a = e.handle(InputEvent::Press(key(A)));
        assert!(a.is_empty(), "ikinci jest başlamamalı: {a:?}");
    }

    /// Öne yüklü eğride sondaki mikro adımlar elenmeli, ama jest yine
    /// hedefe VARMALI ve parmak kalkmalı.
    #[test]
    fn tiny_tail_steps_are_dropped_but_target_is_reached() {
        use crate::input::profile::Easing;
        let mut b = BTreeMap::new();
        b.insert("sol".into(), Binding::Swipe {
            trigger: Trigger::Key(A),
            from: Norm::new(0.5, 0.5), to: Norm::new(0.2, 0.5), duration_ms: 80,
            group: None, easing: Easing::EaseOutStrong,
        });
        let mut e = Engine::new(Profile { name: "t".into(), package: "p".into(), bindings: b });
        let _ = e.handle(InputEvent::Press(key(A)));

        let mut moves: Vec<Norm> = Vec::new();
        let mut lifted = false;
        for ms in (0..=100).step_by(2) {
            for act in e.tick(ms) {
                match act {
                    TouchAction::Move { at, .. } => moves.push(at),
                    TouchAction::Up { .. } => lifted = true,
                    _ => {}
                }
            }
        }
        assert!(lifted, "parmak kalkmalı");
        let last = moves.last().expect("hareket yok");
        assert!((last.x - 0.2).abs() < 1e-4, "hedefe varmalı: {last:?}");

        // Ardışık adımlar arasında (son hariç) anlamsız mesafe kalmamalı.
        for w in moves.windows(2).take(moves.len().saturating_sub(2)) {
            let d = ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt();
            assert!(d >= MIN_STEP_DELTA * 0.99,
                "anlamsız ara adım kaldı: {d} < {MIN_STEP_DELTA}");
        }
    }



    /// Boşta ortalama: kalkış ve iniş AYRI karelerde olmalı.
    ///
    /// Tek dizide dönerlerse arka uç ikisini tek `SYN_REPORT`'a koyar;
    /// Android dokunuşun bittiğini göremez ve oyun ışınlanma görür. Kodun
    /// başka yerinde defalarca uyarılan hata buradaydı.
    #[test]
    fn idle_recenter_lifts_and_lands_in_separate_frames() {
        let mut e = always_on_aim();
        let _ = e.tick(0);
        let _ = mouse(&mut e, 300.0, 0.0);
        // Hemen: henüz boşta değil.
        assert!(e.tick(10).is_empty(), "hareketten hemen sonra ortalanmamalı");

        // Boşta: yalnızca KALKIŞ.
        let a = e.tick(100);
        assert!(matches!(a[..], [TouchAction::Up { .. }]),
                "önce yalnızca kalkış olmalı: {a:?}");
        assert!(e.has_pending(), "gecikmeli iniş bekleniyor sayılmalı");

        // Sonraki kare: yalnızca İNİŞ, ve merkeze.
        let b = e.tick(101);
        match b[..] {
            [TouchAction::Down { at, .. }] => {
                assert!((at.x - 0.5).abs() < 1e-4 && (at.y - 0.5).abs() < 1e-4,
                        "merkeze inmeli: {at:?}");
            }
            _ => panic!("ayrı karede yalnızca iniş bekleniyordu: {b:?}"),
        }
    }

    // ---------------------------------------------------------------
    // SINIRSIZ NİŞAN
    //
    // Kullanıcının bildirdiği üç belirtinin de kaynağı kenarda
    // sıfırlamaydı: parmak kalkınca oyun takibi kesiyor ("hiç
    // algılamıyor"), gecikmeli iniş kendini besleyince saniyeler süren
    // ölü bölge çıkıyor, doğrusal olmayan ölçekleme de hassasiyeti
    // parmağın görünmeyen konumuna bağlıyordu ("aim kayıyor").
    //
    // Sınırsız kipte sıfırlama YOK. Aşağıdakiler bunu koruyor.
    // ---------------------------------------------------------------

    fn free_aim() -> Engine {
        let mut b = BTreeMap::new();
        b.insert("bakis".into(), Binding::Aim {
            toggle: None,
            origin: Norm::new(0.72, 0.5),
            sensitivity: 0.001,
            deadzone: 0.5,
            recenter_margin: 0.12,
            handoff: false, nonlinear: true, reset_delay_ms: 12,
            unbounded: true, safety_span: 32.0,
        });
        let mut e = Engine::new(Profile {
            name: "t".into(), package: "p".into(), bindings: b });
        e.set_offscreen_ok(true);
        e
    }

    /// Sürekli çevirmede parmak ASLA kalkmamalı ve ekran dışına çıkmalı.
    ///
    /// Sınırlı yolda aynı hareket onlarca kalkış/iniş üretiyordu; her biri
    /// oyunun takibini kesiyor ve bir kare dönüş yutuyordu.
    #[test]
    fn unbounded_aim_never_lifts_the_finger() {
        let mut e = free_aim();
        let mut last = Norm::new(0.0, 0.0);
        for _ in 0..400 {
            for a in mouse(&mut e, 40.0, 0.0) {
                assert!(!matches!(a, TouchAction::Up { .. }),
                        "sınırsız kipte parmak kalkmamalı");
                if let TouchAction::Move { at, .. } = a { last = at; }
            }
        }
        assert!(last.is_offscreen(), "parmak ekran dışına çıkabilmeli: {last:?}");
        assert!(last.x > 5.0, "400 × 40 sayım × 0.001 ≈ 16 ekran: {last:?}");
        assert_eq!(e.active_pointers(), 1, "tek nişan parmağı kalmalı");
    }

    /// Hassasiyet SABİT olmalı: aynı fare hareketi her konumda aynı
    /// mesafeyi katetmeli. Değişirse kas hafızası kurulamaz.
    #[test]
    fn unbounded_aim_sensitivity_does_not_drift() {
        let mut e = free_aim();
        let step_at = |e: &mut Engine| -> f32 {
            let before = e.aim_position().expect("parmak inmiş olmalı").x;
            let _ = mouse(e, 50.0, 0.0);
            e.aim_position().unwrap().x - before
        };
        let _ = mouse(&mut e, 50.0, 0.0);   // iniş
        let _ = mouse(&mut e, 50.0, 0.0);   // ertelenen uygulanır
        let near = step_at(&mut e);
        for _ in 0..200 { let _ = mouse(&mut e, 50.0, 0.0); }
        let far = step_at(&mut e);
        assert!((near - far).abs() < 1e-5,
                "merkezde {near}, uzakta {far} — hassasiyet değişmemeli");
        assert!((near - 0.05).abs() < 1e-5, "50 sayım × 0.001 = 0.05: {near}");
    }

    /// İniş ile ilk hareket AYNI karede olmamalı: oyun delta hesaplayamaz
    /// ve o dönüş kaybolur.
    #[test]
    fn unbounded_aim_lands_alone_then_moves() {
        let mut e = free_aim();
        let first = mouse(&mut e, 60.0, 0.0);
        match first[..] {
            [TouchAction::Down { at, .. }] => {
                assert!((at.x - 0.72).abs() < 1e-4, "bakış bölgesine inmeli: {at:?}");
            }
            _ => panic!("önce yalnızca iniş bekleniyordu: {first:?}"),
        }
        let second = e.tick(e.now_ms() + 1);
        match second[..] {
            [TouchAction::Move { at, .. }] =>
                assert!((at.x - (0.72 + 0.06)).abs() < 1e-4,
                        "ertelenen hareket kaybolmamalı: {at:?}"),
            _ => panic!("sonra hareket bekleniyordu: {second:?}"),
        }
    }

    /// Arka uç ekran dışını taşıyamıyorsa sınırsız kip DEVREYE GİRMEMELİ.
    ///
    /// uinput yolunda libinput koordinatı ekrana sıkıştırır; sınırsız kipe
    /// güvenmek parmağı kenarda sonsuza kadar takılı bırakırdı — sınırlı
    /// yoldan bile kötü.
    #[test]
    fn unbounded_needs_backend_support() {
        let mut e = free_aim();
        e.set_offscreen_ok(false);
        assert!(!e.aim_is_unbounded());
        let mut saw_lift = false;
        for _ in 0..400 {
            for a in mouse(&mut e, 40.0, 0.0) {
                if matches!(a, TouchAction::Up { .. }) { saw_lift = true; }
                if let TouchAction::Move { at, .. } = a {
                    assert!(!at.is_offscreen(),
                            "desteklenmeyen arka uçta ekran dışına çıkılmamalı: {at:?}");
                }
            }
        }
        assert!(saw_lift, "sınırlı yolda sıfırlama beklenir");
    }

    /// Emniyet kutusuna dayanılsa bile sıfırlama YALNIZCA fare dururken.
    ///
    /// Hareket sırasında sıfırlamak duraklama olarak hissedilir; durakta
    /// yapılırsa hiç fark edilmez.
    #[test]
    fn safety_reset_waits_for_the_mouse_to_stop() {
        let mut e = free_aim();
        // Kutunun yarısını (16 ekran) kesin olarak aş.
        for _ in 0..600 { let _ = mouse(&mut e, 50.0, 0.0); }
        let far = e.aim_position().expect("parmak inmiş olmalı");
        assert!(far.x - 0.72 > 16.0, "eşiğin ötesinde olmalı: {far:?}");

        // Fare hâlâ hareket ediyor: sıfırlama yok.
        for a in mouse(&mut e, 50.0, 0.0) {
            assert!(!matches!(a, TouchAction::Up { .. }),
                    "hareket sırasında sıfırlanmamalı");
        }
        // Fare durdu: kalkış, ardından ayrı karede iniş.
        let lift = e.tick(e.now_ms() + 200);
        assert!(matches!(lift[..], [TouchAction::Up { .. }]),
                "boşta sıfırlanmalı: {lift:?}");
        let land = e.tick(e.now_ms() + 20);
        match land[..] {
            [TouchAction::Down { at, .. }] =>
                assert!((at.x - 0.72).abs() < 1e-4, "merkeze inmeli: {at:?}"),
            _ => panic!("ayrı karede iniş bekleniyordu: {land:?}"),
        }
    }

    /// Merkeze yakınken boşta ortalama YAPILMAMALI — gereksiz Up/Down
    /// bazı oyunlarda dokunuş sanılır.
    #[test]
    fn idle_recenter_skips_when_already_near_centre() {
        let mut e = always_on_aim();
        let _ = e.tick(0);
        let _ = mouse(&mut e, 5.0, 0.0);
        assert!(e.tick(500).is_empty(), "merkeze yakınken ortalanmamalı");
    }

    /// Sıfırlama sonrası bekleyen iş SAYILMALI — sayılmazsa çağıran
    /// tick'i çağırmayı bırakır ve nişan tamamen ölür.
    #[test]
    fn pending_includes_delayed_down() {
        let mut e = always_on_aim();
        let _ = mouse(&mut e, 10.0, 0.0);
        let a = mouse(&mut e, 400.0, 0.0);
        assert!(a.iter().any(|x| matches!(x, TouchAction::Up { .. })));
        assert!(e.has_pending(),
            "gecikmeli iniş bekleniyorken has_pending TRUE olmalı");
    }

    /// Yürürken nişan sıfırlanırsa joystick DURMAMALI.
    #[test]
    fn joystick_keeps_working_during_aim_reset() {
        let mut b = BTreeMap::new();
        b.insert("bakis".into(), Binding::Aim {
            toggle: None, origin: Norm::new(0.5, 0.5),
            sensitivity: 0.001, deadzone: 0.5, recenter_margin: 0.12,
            handoff: false, nonlinear: false, reset_delay_ms: 20,
            unbounded: false, safety_span: 32.0,
        });
        b.insert("hareket".into(), Binding::Joystick {
            up: Trigger::Key(W), down: Trigger::Key(S),
            left: Trigger::Key(A), right: Trigger::Key(D),
            center: Norm::new(0.2, 0.7), radius: 0.1,
        });
        let mut e = Engine::new(Profile {
            name: "t".into(), package: "p".into(), bindings: b });

        let _ = mouse(&mut e, 10.0, 0.0);
        let _ = mouse(&mut e, 400.0, 0.0);      // nişan sıfırlanır
        // Sıfırlama beklenirken joystick basılır.
        let _ = e.handle(InputEvent::Press(key(W)));
        let a = e.tick(e.now_ms() + 1);          // iniş henüz zamanı gelmedi
        assert!(a.iter().any(|x| matches!(x, TouchAction::Move { .. })),
            "nişan sıfırlanırken joystick yönü uygulanmalı: {a:?}");
    }

    /// Kayıp tuş bırakma olayı işaretçi SIZDIRMAMALI.
    ///
    /// Sızıntı birikirse havuz dolar ve nişan işaretçi alamaz — fare
    /// tamamen ölür. Gerçekte yaşandı.
    #[test]
    fn orphaned_pointers_are_reclaimed() {
        let mut e = Engine::new(joystick_profile());
        let _ = e.handle(InputEvent::Press(key(SPACE)));
        assert_eq!(e.active_pointers(), 1);

        // Bırakma olayını "kaybet": held setini doğrudan temizle.
        e.forget_held_for_test();

        let a = e.tick(10);
        assert!(a.iter().any(|x| matches!(x, TouchAction::Up { .. })),
            "sahipsiz işaretçi bırakılmalı: {a:?}");
        assert_eq!(e.active_pointers(), 0, "havuz temizlenmeli");
    }

    #[test]
    fn tick_without_pending_gesture_is_silent() {
        let mut e = swipe_engine();
        assert!(e.tick(1000).is_empty());
    }

    /// Motor kapatılınca devam eden jest de temizlenmeli.
    #[test]
    fn disabling_clears_pending_swipes() {
        let mut e = swipe_engine();
        let _ = e.handle(InputEvent::Press(key(A)));
        assert!(e.has_pending());
        let _ = e.set_enabled(false);
        assert!(!e.has_pending());
    }
}

#[cfg(test)]
mod must_use_guard {
    //! `tick` ve `handle` dönüşlerinin atılması bugün gerçek bir hataya yol
    //! açtı: önceki jestin UP'ı kayboluyor ve parmak ekranda asılı kalıyordu.
    //! `#[must_use]` bunu derleme zamanında yakalar; bu modül niyeti belgeler.
    use super::*;

    #[test]
    fn tick_and_handle_return_actions_that_must_be_dispatched() {
        let mut b = std::collections::BTreeMap::new();
        b.insert("t".into(), Binding::Tap {
            trigger: Trigger::Key(57), at: Norm::new(0.5, 0.5),
        });
        let mut e = Engine::new(Profile {
            name: "t".into(), package: "p".into(), bindings: b,
        });
        let down = e.handle(InputEvent::Press(TriggerKind::Key(57)));
        assert!(!down.is_empty(), "eylemler çağırana DÖNMELİ, içeride tutulmamalı");
        let up = e.handle(InputEvent::Release(TriggerKind::Key(57)));
        assert!(matches!(up[..], [TouchAction::Up { .. }]));
    }
}
