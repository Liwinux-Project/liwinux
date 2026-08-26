// Waydroid penceresini tam ekran yapar ve sonucu liwd'ye BİLDİRİR.
//
// Bildirim şart: KWin scripting API'si script çıktısını çağırana döndürmez.
// Geri bildirim olmadan liwd "oldu mu olmadı mı" bilemez ve sessizce
// yanlış varsayımla devam eder.
//
// Neden tam ekran: dokunuşlar EKRAN uzayında gidiyor. Pencere çıkışla
// hizalı değilse profil koordinatları kayar, kenar dokunuşları dışarı düşer.
// Yalnızca resourceClass'a bakılır.
//
// Başlığa (caption) veya resourceName'e bakmak TEHLİKELİ: kullanıcının
// terminali "waydroid ..." komutunu çalıştırırken başlığında o kelimeyi
// taşır ve yanlışlıkla eşleşir. Gerçekte yaşandı — teşhis aracı kullanıcının
// konsol penceresini Waydroid penceresi sandı. Başlıkla eşleştiren bir
// script o pencereyi tam ekran yapabilirdi.
//
// Birden fazla gerçek eşleşme olursa en büyük alanlı seçilir; listedeki
// sıra KWin'in yığın düzenine bağlı olduğu için belirlenimsizdir.
function liwFindWaydroid(wins) {
    var best = null, bestArea = -1;
    for (var i = 0; i < wins.length; i++) {
        var w = wins[i];
        var cls = (w.resourceClass || "").toString().toLowerCase();
        if (cls !== "waydroid") continue;
        try { if (w.dialog || w.popupWindow || w.transient) continue; } catch (e) {}
        var g = w.frameGeometry;
        var area = g.width * g.height;
        if (area > bestArea) { bestArea = area; best = w; }
    }
    return best;
}

function liwReport(found, x, y, w, h, fs) {
    callDBus("id.liwinux.Manager1", "/id/liwinux/Manager1",
             "id.liwinux.Manager1", "ReportWindowGeometry",
             found, x, y, w, h, fs);
}

var wins = (typeof workspace.windowList === "function")
    ? workspace.windowList()
    : (typeof workspace.stackingOrder !== "undefined" ? workspace.stackingOrder
                                                      : workspace.clientList());

var target = liwFindWaydroid(wins);

if (target === null) {
    liwReport(false, 0, 0, 0, 0, false);
} else {
    if (!target.fullScreen) {
        // Etkinleştir: KWin bazı işlemleri yalnızca etkin pencerede uygular.
        try { workspace.activeWindow = target; }
        catch (e) { try { workspace.activeClient = target; } catch (e2) {} }
        try { target.setMaximize(true, true); } catch (e) {}
        target.fullScreen = true;
    }
    var g = target.frameGeometry;
    liwReport(true, g.x, g.y, g.width, g.height, target.fullScreen);
}
