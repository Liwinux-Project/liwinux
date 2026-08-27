// Waydroid penceresinin durumunu liwd'ye BİLDİRİR — hiçbir şeyi değiştirmez.
//
// Neden ayrı bir script: fullscreen.js pencereyi tam ekran YAPAR. Durumu
// öğrenmek için onu çağırmak, sırf bakmak isterken kullanıcının kasten
// çıktığı tam ekranı geri zorlamak demekti.
//
// liwd bunu düzenli aralıkla çağırır. Pencere KAYBOLDUĞUNDA tam ekran
// bayrağını sıfırlar; böylece `show-full-ui` kapatılıp yeniden açılınca
// yeni pencere tekrar tam ekran yapılır. Session'ın durmasını beklemek
// yetmiyordu: kullanıcı session'ı durdurmadan pencereyi kapatıp açıyor.

// Yalnızca resourceClass'a bakılır.
//
// Başlığa (caption) bakmak TEHLİKELİ: kullanıcının terminali "waydroid ..."
// komutunu çalıştırırken başlığında o kelimeyi taşır. Gerçekte yaşandı.
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

var wins = (typeof workspace.windowList === "function")
    ? workspace.windowList()
    : (typeof workspace.stackingOrder !== "undefined" ? workspace.stackingOrder
                                                      : workspace.clientList());

var t = liwFindWaydroid(wins);
if (t === null) {
    callDBus("id.liwinux.Manager1", "/id/liwinux/Manager1",
             "id.liwinux.Manager1", "ReportWindowGeometry",
             false, 0, 0, 0, 0, false);
} else {
    var g = t.frameGeometry;
    callDBus("id.liwinux.Manager1", "/id/liwinux/Manager1",
             "id.liwinux.Manager1", "ReportWindowGeometry",
             true, g.x, g.y, g.width, g.height, t.fullScreen);
}
