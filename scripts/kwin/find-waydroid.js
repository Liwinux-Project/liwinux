// Waydroid penceresini bulur, geometrisini bildirir ve tam ekran yapar.
// KWin scripting API'si sürümler arasında değişti; ikisini de dene.
var wins = (typeof workspace.windowList === "function")
    ? workspace.windowList()
    : (typeof workspace.stackingOrder !== "undefined" ? workspace.stackingOrder
                                                      : workspace.clientList());

print("LIWINUX: toplam pencere = " + wins.length);
var found = 0;
for (var i = 0; i < wins.length; i++) {
    var w = wins[i];
    var cls = (w.resourceClass || "").toString().toLowerCase();
    var nam = (w.resourceName  || "").toString().toLowerCase();
    var cap = (w.caption       || "").toString().toLowerCase();
    print("LIWINUX: pencere cls='" + cls + "' name='" + nam + "' cap='" + cap + "'");
    if (cls.indexOf("waydroid") >= 0 || nam.indexOf("waydroid") >= 0 ||
        cap.indexOf("waydroid") >= 0 || cls.indexOf("android") >= 0) {
        var g = w.frameGeometry;
        print("LIWINUX: BULUNDU geometri=" + g.x + "," + g.y + " " + g.width + "x" + g.height
              + " tamekran=" + w.fullScreen);
        w.fullScreen = true;
        var g2 = w.frameGeometry;
        print("LIWINUX: TAMEKRAN_SONRASI=" + g2.x + "," + g2.y + " " + g2.width + "x" + g2.height);
        found++;
    }
}
if (found === 0) print("LIWINUX: Waydroid penceresi BULUNAMADI");
