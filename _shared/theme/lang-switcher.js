// Language switcher injected into mdBook's menu bar.
//
// Detects the current locale from the URL by finding a path segment that
// matches one of LOCALES, then renders a dropdown linking to the same page
// in every other locale.
//
// LOCALES is generated from locales.toml at build time by `cargo mdbook build`.
// Edit locales.toml at the repo root to add or remove locales.
(function () {
  const LOCALES = [
    { code: "en", label: "English" },
    { code: "fr", label: "Français" },
    { code: "ja", label: "日本語" },
    { code: "es", label: "Español" },
    { code: "zh-CN", label: "中文" },
  ];

  const pathSegments = window.location.pathname.split("/");
  const localeIndex = pathSegments.findIndex((seg) =>
    LOCALES.some((l) => l.code === seg)
  );
  if (localeIndex < 0) return;
  const currentLocale = pathSegments[localeIndex];
  const currentLabel = LOCALES.find((l) => l.code === currentLocale)?.label ?? currentLocale.toUpperCase();

  const urlForLocale = (code) => {
    const next = pathSegments.slice();
    next[localeIndex] = code;
    return next.join("/");
  };

  const menuRight = document.querySelector(".menu-bar .right-buttons");
  if (!menuRight) return;

  // Wrapper provides the `position: relative` anchor for the dropdown.
  const wrapper = document.createElement("div");
  wrapper.style.position = "relative";
  wrapper.style.display = "inline-flex";
  wrapper.style.alignItems = "center";

  const button = document.createElement("button");
  button.id = "language-toggle";
  button.className = "icon-button";
  button.type = "button";
  button.title = "Change language";
  button.setAttribute("aria-label", "Change language");
  button.setAttribute("aria-haspopup", "true");
  button.setAttribute("aria-expanded", "false");
  button.setAttribute("aria-controls", "language-list");
  button.style.fontWeight = "bold";
  button.style.fontSize = "0.8em";
  button.style.letterSpacing = "0.03em";
  button.textContent = currentLabel;

  const list = document.createElement("ul");
  list.id = "language-list";
  list.className = "theme-popup";
  list.setAttribute("aria-label", "Languages");
  list.setAttribute("role", "menu");
  list.style.display = "none";
  list.style.position = "absolute";
  list.style.top = "100%";
  list.style.right = "0";
  list.style.left = "auto";
  list.style.zIndex = "1000";
  list.style.minWidth = "8em";

  for (const { code, label } of LOCALES) {
    const li = document.createElement("li");
    li.setAttribute("role", "none");
    if (code === currentLocale) li.classList.add("theme-selected");

    const link = document.createElement("a");
    link.className = "theme";
    link.setAttribute("role", "menuitem");
    link.textContent = label;
    link.href = urlForLocale(code);
    li.appendChild(link);
    list.appendChild(li);
  }

  button.addEventListener("click", (event) => {
    event.stopPropagation();
    const open = list.style.display === "block";
    list.style.display = open ? "none" : "block";
    button.setAttribute("aria-expanded", String(!open));
  });

  document.addEventListener("click", (event) => {
    if (!wrapper.contains(event.target)) {
      list.style.display = "none";
      button.setAttribute("aria-expanded", "false");
    }
  });

  wrapper.appendChild(button);
  wrapper.appendChild(list);
  menuRight.prepend(wrapper);
})();
