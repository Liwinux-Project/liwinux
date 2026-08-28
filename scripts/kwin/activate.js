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


// RAISES and focuses the Waydroid window.
//
// A separate script because fullscreen.js only activated the window when it was
// NOT already fullscreen, skipping it otherwise. Activation is mandatory before
// taking a screenshot — `spectacle -a` captures the active window, and if that
// is the terminal you get a picture of the terminal. This actually happened.
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
