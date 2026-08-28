// Writes the Waydroid window's and the desktop's geometry to a file.
// A file rather than print() into the journal: parsing is reliable.
var wins = (typeof workspace.windowList === "function")
    ? workspace.windowList()
    : (typeof workspace.stackingOrder !== "undefined" ? workspace.stackingOrder
                                                      : workspace.clientList());
// resourceClass only. Matching the caption matched the user's terminal while
// it displayed a "waydroid ..." command — this actually happened.
var out = "";
for (var i = 0; i < wins.length; i++) {
    var w = wins[i];
    var cls = (w.resourceClass || "").toString().toLowerCase();
    if (cls === "waydroid") {
        var g = w.frameGeometry;
        out += "window=" + g.x + "," + g.y + "," + g.width + "," + g.height + "\n";
        out += "fullscreen=" + w.fullScreen + "\n";
        break;
    }
}
var area = workspace.workspaceSize || workspace.virtualScreenSize;
if (area) out += "desktop=" + area.width + "," + area.height + "\n";
print("LIWINUX_GEOM_BEGIN");
print(out);
print("LIWINUX_GEOM_END");
