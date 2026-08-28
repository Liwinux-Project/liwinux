// Reports the Waydroid window's state to liwd — changes NOTHING.
//
// Why a separate script: fullscreen.js MAKES the window fullscreen. Calling it
// just to learn the current state would force fullscreen back on a user who
// deliberately left it.
//
// liwd calls this periodically. When the window DISAPPEARS it clears the
// fullscreen flag, so that closing and reopening `show-full-ui` gets the new
// window made fullscreen again. Waiting for the session to stop was not enough:
// users close and reopen the window without stopping the session.
// Match on resourceClass ONLY.
//
// Matching the caption or resourceName is DANGEROUS: the user's terminal
// carries that word in its title while running a "waydroid ..." command and
// would match by accident. This actually happened — a diagnostic tool mistook
// the user's console window for the Waydroid window. A script matching on the
// caption could have made that window fullscreen.
//
// If several genuine matches exist, the largest by area wins; list order
// depends on KWin's stacking order and is therefore non-deterministic.
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
