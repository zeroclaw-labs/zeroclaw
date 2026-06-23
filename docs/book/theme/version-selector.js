// Version selector injected into mdBook's menu bar.
//
// Detects the current version from the URL path (e.g., /v0.7.5/, /main/),
// fetches /versions.json from the domain root to list all available versions,
// then renders a dropdown linking to the same page in each version.
(function () {
  // Parse the current version from URL path.
  // Expected formats: /v0.7.5/en/..., /master/en/...
  const pathSegments = window.location.pathname.split("/").filter((s) => s);
  if (pathSegments.length < 2) return; // Not in a versioned docs path

  const versionPattern = /^v\d+\.\d+\.\d+(-[a-z0-9.-]+)?$/i;

  let currentVersion = null;
  let versionIndex = -1;

  for (let i = 0; i < pathSegments.length; i++) {
    const seg = pathSegments[i];
    if (seg === "master" || versionPattern.test(seg)) {
      currentVersion = seg;
      versionIndex = i;
      break;
    }
  }

  if (!currentVersion) return;

  // Fetch versions.json relative to the base path (supports subdirectory/project page hosting).
  const basePath = "/" + pathSegments.slice(0, versionIndex).map((s) => s + "/").join("");
  fetch(basePath + "versions.json", { cache: "no-cache" })
    .then((response) => {
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      return response.json();
    })
    .then((data) => {
      const versions = data.versions || [];
      if (versions.length === 0) return;

      const menuRight = document.querySelector(".menu-bar .right-buttons");
      if (!menuRight) return;

      // Find the label for the current version
      const currentVersionObj = versions.find((v) => v.tag === currentVersion);
      const currentLabel = currentVersionObj?.label || currentVersion;

      // Build URL for another version by replacing the version segment
      const urlForVersion = (tag) => {
        const next = pathSegments.slice();
        next[versionIndex] = tag;
        return "/" + next.join("/") + window.location.hash;
      };

      // Wrapper provides the `position: relative` anchor for the dropdown.
      const wrapper = document.createElement("div");
      wrapper.style.position = "relative";
      wrapper.style.display = "inline-flex";
      wrapper.style.alignItems = "center";
      wrapper.style.marginRight = "0.5em";

      // Button showing current version
      const button = document.createElement("button");
      button.id = "version-toggle";
      button.className = "icon-button";
      button.type = "button";
      button.title = "Change documentation version";
      button.setAttribute("aria-label", "Documentation version: " + currentLabel);
      button.setAttribute("aria-haspopup", "true");
      button.setAttribute("aria-expanded", "false");
      button.setAttribute("aria-controls", "version-list");
      button.style.fontWeight = "bold";
      button.style.fontSize = "0.75em";
      button.style.letterSpacing = "0.03em";
      button.textContent = currentLabel;

      // Dropdown list
      const list = document.createElement("ul");
      list.id = "version-list";
      list.className = "theme-popup";
      list.setAttribute("aria-label", "Documentation versions");
      list.setAttribute("role", "menu");
      list.style.display = "none";
      list.style.position = "absolute";
      list.style.top = "100%";
      list.style.right = "0";
      list.style.left = "auto";
      list.style.zIndex = "1000";
      list.style.minWidth = "12em";

      // Populate dropdown with all versions
      for (const version of versions) {
        const li = document.createElement("li");
        li.setAttribute("role", "none");
        if (version.tag === currentVersion) {
          li.classList.add("theme-selected");
        }

        const link = document.createElement("a");
        link.className = "theme";
        link.setAttribute("role", "menuitem");
        link.textContent = version.label;
        link.href = urlForVersion(version.tag);
        li.appendChild(link);
        list.appendChild(li);
      }

      // Toggle dropdown on button click
      button.addEventListener("click", (event) => {
        event.stopPropagation();
        const open = list.style.display === "block";
        list.style.display = open ? "none" : "block";
        button.setAttribute("aria-expanded", String(!open));
      });

      // Close dropdown when clicking elsewhere
      document.addEventListener("click", (event) => {
        if (!wrapper.contains(event.target)) {
          list.style.display = "none";
          button.setAttribute("aria-expanded", "false");
        }
      });

      wrapper.appendChild(button);
      wrapper.appendChild(list);
      // Insert before language switcher
      menuRight.prepend(wrapper);
    })
    .catch((err) => {
      // Silently fail if versions.json is not available
      console.debug("version-selector: could not fetch /versions.json", err);
    });
})();
