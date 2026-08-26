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


// Waydroid penceresini ÖNE GETİRİR ve odaklar.
//
// Ayrı script: fullscreen.js pencereyi yalnızca tam ekran DEĞİLSE
// aktifleştiriyordu, zaten tam ekransa atlıyordu. Ekran görüntüsü almadan
// önce aktifleştirme şart — `spectacle -a` aktif pencereyi yakalar ve
// aktif pencere terminal olursa terminalin görüntüsü alınır. Gerçekte oldu.
var wins = (typeof workspace.windowList === "function")
    ? workspace.windowList()
    : (typeof workspace.stackingOrder !== "undefined" ? workspace.stackingOrder
                                                      : workspace.clientList());
var t = liwFindWaydroid(wins);
var ok = false;
if (t !== null) {
    try { workspace.activeWindow = t; ok = true; }
    catch (e) { try { workspace.activeClient = t; ok = true; } catch (e2) {} }
    try { workspace.raiseWindow(t); } catch (e) {}
    if (t.minimized) { t.minimized = false; }
}
var g = ok ? t.frameGeometry : { x: 0, y: 0, width: 0, height: 0 };
callDBus("id.liwinux.Manager1", "/id/liwinux/Manager1",
         "id.liwinux.Manager1", "ReportWindowGeometry",
         ok, g.x, g.y, g.width, g.height, ok ? t.fullScreen : false);
