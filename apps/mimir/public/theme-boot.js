// Runs before the stylesheet draws anything: puts the saved accent and theme on
// <html>, so a chosen colour never flashes the default sea on startup.
//
// A separate file rather than an inline <script> because the CSP is
// `script-src 'self'`. It only replays what src/accent.ts computed and stored;
// no colour maths happens here.
(function () {
  var root = document.documentElement;
  var scheme = "system";
  try {
    var t = JSON.parse(localStorage.getItem("mimir.theme"));
    if (t && t.vars) {
      for (var k in t.vars) if (k.indexOf("--u-") === 0) root.style.setProperty(k, t.vars[k]);
    }
    if (t && (t.scheme === "dark" || t.scheme === "light")) scheme = t.scheme;
  } catch (e) {}
  if (scheme === "system") {
    scheme = matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  }
  root.dataset.theme = scheme;
})();
