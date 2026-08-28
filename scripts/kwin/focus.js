// Reports focus changes to liwd.
//
// Why this is needed: Android does NOT know the window was minimised on the
// host. Even with the game in the background Android still considers it
// foreground, the profile stays active, and touches are delivered in screen
// space — landing on the user's real desktop.
//
// The KWin API changed between versions (clientActivated -> windowActivated);
// we connect to both.
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

// Report the initial state too, otherwise the focus held before the script
// loaded is lost.
var cur = (typeof workspace.activeWindow !== "undefined")
    ? workspace.activeWindow
    : (typeof workspace.activeClient !== "undefined" ? workspace.activeClient : null);
liwReport(cur);
