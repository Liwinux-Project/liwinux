// Makes the Waydroid window fullscreen and REPORTS the result to liwd.
//
// Reporting is mandatory: the KWin scripting API does not return script output
// to the caller. Without feedback liwd cannot know whether it worked, and would
// silently carry on with a wrong assumption.
//
// Why fullscreen: touches are delivered in SCREEN space. If the window is not
// aligned with the output, profile coordinates shift and edge touches fall
// outside it.
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
        // Activate first: KWin applies some operations only to the active window.
        try { workspace.activeWindow = target; }
        catch (e) { try { workspace.activeClient = target; } catch (e2) {} }
        try { target.setMaximize(true, true); } catch (e) {}
        target.fullScreen = true;
    }
    var g = target.frameGeometry;
    liwReport(true, g.x, g.y, g.width, g.height, target.fullScreen);
}
