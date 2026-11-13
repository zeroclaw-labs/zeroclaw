// Redirect script of the docs site's root 404.html. `gen-root-404`
// (versions.rs) writes it after two lines that set `target` (the stable
// alias, or master when no stable pointer resolves) and `locales` (the stable
// release's locale directories). scripts/docs/promote_stable.py rewrites
// those two lines when it promotes a release.
//
// A missing /<version>/<locale>/<page> (a pruned release) or /<locale>/<page>
// (the pre-versioned layout) goes to the same page under `target`. The version
// pattern takes the tag forms the deploy workflow accepts. Any other missing
// path stays on the page's message.
(function () {
  var version = /^(master|v\d+\.\d+\.\d+(-[A-Za-z0-9._-]+)?)$/;
  var segments = location.pathname.split("/").filter(Boolean);
  var rest = null;
  if (segments.length >= 2 && version.test(segments[0]) && locales.indexOf(segments[1]) >= 0) {
    rest = segments.slice(1);
  } else if (segments.length >= 1 && locales.indexOf(segments[0]) >= 0) {
    rest = segments;
  }
  if (!rest) return;
  var path = "/" + target + "/" + rest.join("/");
  if (location.pathname.slice(-1) === "/") path += "/";
  if (path === location.pathname) return;
  location.replace(path + location.search + location.hash);
})();
