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

// Diagnostic: list every window, flag the Waydroid one.
var wins = (typeof workspace.windowList === "function")
    ? workspace.windowList()
    : (typeof workspace.stackingOrder !== "undefined" ? workspace.stackingOrder
                                                      : workspace.clientList());

print("LIWINUX: total windows = " + wins.length);
for (var i = 0; i < wins.length; i++) {
    var w = wins[i];
    print("LIWINUX: window cls='" + (w.resourceClass || "") +
          "' cap='" + (w.caption || "") + "'");
}

var t = liwFindWaydroid(wins);
if (t === null) {
    print("LIWINUX: Waydroid window NOT FOUND");
} else {
    var g = t.frameGeometry;
    print("LIWINUX: FOUND geometry=" + g.x + "," + g.y + " " +
          g.width + "x" + g.height + " fullscreen=" + t.fullScreen);
}
