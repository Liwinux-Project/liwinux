// Waydroid penceresinin ve masaüstünün geometrisini bir dosyaya yazar.
// print() ile journal'a yazmak yerine dosya: ayrıştırması güvenilir.
var wins = (typeof workspace.windowList === "function")
    ? workspace.windowList() : workspace.clientList();
// Yalnızca resourceClass. Başlığa bakmak kullanıcının "waydroid ..."
// yazdığı terminali eşleştiriyordu — gerçekte yaşandı.
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
