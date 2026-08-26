// Odak değişimlerini liwd'ye bildirir.
//
// Neden gerekli: Android, pencerenin host'ta küçültüldüğünü BİLMEZ. Oyun alt
// tabdayken bile Android onu ön planda sanar, profil etkin kalır ve dokunuşlar
// ekran uzayında gidip kullanıcının gerçek masaüstüne düşer.
//
// KWin API'si sürümler arasında değişti (clientActivated -> windowActivated);
// ikisine de bağlanıyoruz.
function liwReport(w) {
    var cls = "";
    try { cls = w ? (w.resourceClass || "").toString().toLowerCase() : ""; }
    catch (e) { cls = ""; }
    callDBus("id.liwinux.Manager1", "/id/liwinux/Manager1",
             "id.liwinux.Manager1", "SetActiveWindow", cls);
}

if (typeof workspace.windowActivated !== "undefined") {
    workspace.windowActivated.connect(liwReport);
} else if (typeof workspace.clientActivated !== "undefined") {
    workspace.clientActivated.connect(liwReport);
}

// Başlangıç durumunu da bildir; yoksa script yüklenene kadarki odak kaybolur.
var cur = (typeof workspace.activeWindow !== "undefined")
    ? workspace.activeWindow
    : (typeof workspace.activeClient !== "undefined" ? workspace.activeClient : null);
liwReport(cur);
