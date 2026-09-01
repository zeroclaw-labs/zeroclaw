// The desktop rail's overflow contract. The rail tooltip is rendered into
// `document.body` via `createPortal` (see `RailNavItem`), so nothing in the
// rail's DOM subtree ever extends past the rail's right border. `overflow-y:
// auto` alone therefore keeps the nav at `scrollWidth === clientWidth`, and
// no `overflow-x` value (hidden or clip) may be applied to either the
// `<nav>` or the `<aside>` as a horizontal-overflow mask.
export const railNavClassName = "flex-1 overflow-y-auto py-3 px-1.5";

// The desktop rail's link sizing contract. Links are `w-full max-w-10 mx-auto` —
// never a bare fixed `w-10` — so they shrink to the available width when a
// classic vertical scrollbar gutter consumes part of the 56px rail (`w-14`).
// With an overlay scrollbar the nav keeps 44px of content width and links cap
// at their 40px `max-w-10`; `mx-auto` keeps the 2px of spare horizontal space
// evenly split so the icon, hover background, and active bar stay centered in
// the rail, matching the original `w-10 mx-auto` on `master`. With a classic
// gutter the content width drops to ~34px and the `w-full` links shrink instead
// of overflowing horizontally, keeping the nav at `scrollWidth === clientWidth`
// in both cases. The 22px icon stays centered in either case.
export const railLinkClassName =
  "group relative flex h-10 w-full max-w-10 mx-auto items-center justify-center";

export const railAsideStyle = {
  background: "var(--pc-bg-sidebar)",
  borderColor: "var(--pc-border)",
} as const;
