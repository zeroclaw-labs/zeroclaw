// The desktop rail's overflow contract. The rail tooltip is rendered into
// `document.body` via `createPortal` (see `RailNavItem`), so nothing in the
// rail's DOM subtree ever extends past the rail's right border. `overflow-y:
// auto` alone therefore keeps the nav at `scrollWidth === clientWidth`, and
// no `overflow-x` value (hidden or clip) may be applied to either the
// `<nav>` or the `<aside>` as a horizontal-overflow mask.
export const railNavClassName = "flex-1 overflow-y-auto py-3 px-1.5";

export const railAsideStyle = {
  background: "var(--pc-bg-sidebar)",
  borderColor: "var(--pc-border)",
} as const;
