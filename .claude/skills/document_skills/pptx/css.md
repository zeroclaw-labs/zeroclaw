# Global CSS Framework Reference

This document provides a comprehensive reference for the global.css framework used in HTML slide creation for PowerPoint conversion.

---

## ⚠️ No Import Necessary

The global.css framework is automatically added to every slide. Do NOT try to include it in a slide with `<style>` or `<link>` tags.

---

## Overview

The global.css framework is designed specifically for creating HTML slides that convert cleanly to PowerPoint presentations. It provides:

- **Fixed slide dimensions** (960×540px, 16:9 aspect ratio)
- **Consistent design system** with predefined colors, typography, and spacing
- **Flexbox-based layout system** for responsive slide content
- **Utility-first approach** for rapid slide development
- **Professional styling** optimized for business presentations

## Design System Variables

### Typography Variables

```css
/* Headings */
--font-family-display: Arial, sans-serif;
--font-weight-display: 600;

/* Body text */
--font-family-content: Arial, sans-serif;
--font-weight-content: 400;
--font-size-content: 16px;
--line-height-content: 1.4;
```

### Color Palette

#### Surface Colors

- `--color-surface`: `#ffffff` - Default background
- `--color-surface-foreground`: `#1d1d1d` - Text on default background

#### Primary Colors

- `--color-primary`: `#1791e8` - Primary actions/accents
- `--color-primary-light`: Lightened primary (10% white mix)
- `--color-primary-dark`: Darkened primary (10% black mix)
- `--color-primary-foreground`: `#fafafa` - Text on primary background

#### Secondary Colors

- `--color-secondary`: `#f5f5f5` - Secondary actions
- `--color-secondary-foreground`: `#171717` - Text on secondary background

#### Utility Colors

- `--color-muted`: `#f5f5f5` - Subtle backgrounds
- `--color-muted-foreground`: `#737373` - Muted text
- `--color-accent`: `#f5f5f5` - Accent elements
- `--color-accent-foreground`: `#171717` - Text on accent background
- `--color-border`: `#c8c8c8` - Border elements

### Color Utility Classes

**Background:** `.bg-surface`, `.bg-primary`, `.bg-secondary`, `.bg-muted`, `.bg-accent`, `.bg-border`
**Text:** `.text-surface-foreground`, `.text-primary`, `.text-muted-foreground`, etc.
_Uses the color variables defined above except `*-light` and `*-dark`_

### Spacing & Layout

- `--spacing`: `0.25rem` - Base spacing unit
- `--gap`: `calc(var(--spacing) * 4)` - Standard gap (1rem)
- `--radius`: `0.4rem` - Standard border radius
- `--radius-pill`: `999em` - Pill-shaped border radius

## Slide Structure

### Fixed Dimensions

```css
body {
  width: 960px;
  height: 540px;
  overflow: hidden; /* Prevents content overflow */
}
```

## Layout System

### Container Classes

#### `.row` - Horizontal Layout

- `flex-direction: row`
- `align-items: center`
- `justify-content: stretch`
- Children with `.fill-width` class expand to fill available width
- Children with `.fill-height` class stretch to fill available height

#### `.col` - Vertical Layout

- `flex-direction: column`
- `align-items: stretch`
- `justify-content: center`
- Children with `.fill-height` class expand to fill available height
- Children with `.fill-width` class stretch to fill available width

### Flex Item Behavior

#### `.fill-width` and `.fill-height` - Expandable Elements

- `.fill-width`: `flex: 1` in row containers (expands to fill available width)
- `.fill-height`: `flex: 1` in column containers (expands to fill available height)
- Cross-axis variants also apply `align-self: stretch`
- **Required** for elements that should expand within flex containers
- Use for main content areas

#### `.items-fill-width` and `.items-fill-height` - Auto-Expanding Children

- `.items-fill-width`: Makes all direct children expandable horizontally (`flex: 1`)
- `.items-fill-height`: Makes all direct children expandable vertically (`flex: 1`)
- Cross-axis variants also apply `align-self: stretch` to children
- Convenient alternative to adding `.fill-width`/`.fill-height` class to each child
- Use when all children should expand equally

#### `.fit`, `.fit-width`, and `.fit-height` - Fixed-Size Elements

- `flex: none` (maintains natural size)
- `align-self: auto` (uses parent's align-items value)
- **Default behavior** for elements without `.fill-width`/`.fill-height` classes
- `.fit-width`: axis-specific for row containers (prevents horizontal expansion)
- `.fit-height`: axis-specific for column containers (prevents vertical expansion)
- Use for elements with fixed size inside `.items-fill-width`/`.items-fill-height` containers

#### `.center` - Center Content

- Centers content both horizontally and vertically

### Example Layout Structure

```html
<body class="col">
  <header>Fixed header</header>
  <main class="fill-height row">
    <aside>Sidebar</aside>
    <section class="fill-width">Main content</section>
  </main>
  <footer>Fixed footer</footer>
</body>
```

## Typography Scale

### Text Sizes

- `.text-xs`: `0.75rem` (12px)
- `.text-sm`: `0.875rem` (14px)
- `.text-base`: `1rem` (16px)
- `.text-lg`: `1.125rem` (18px)
- `.text-xl`: `1.25rem` (20px)
- `.text-2xl`: `1.5rem` (24px)
- `.text-3xl`: `1.875rem` (30px)
- `.text-4xl`: `2.25rem` (36px)
- `.text-5xl`: `3rem` (48px)
- `.text-6xl`: `3.75rem` (60px)
- `.text-7xl`: `4.5rem` (72px)
- `.text-8xl`: `6rem` (96px)

## Utility Classes

### Alignment Classes

**text-align**: `.text-left/right/center`
**align-items**: `.items-start/center/baseline/stretch/end`
**align-self**: `.self-start/center/end`
**justify-content**: `.justify-start/center/end`

### Spacing

#### Gap Classes

- `.gap-sm`: Half standard gap
- `.gap`: Standard gap (1rem)
- `.gap-lg`: Double standard gap
- `.gap-xl`: Triple standard gap
- `.gap-2xl`: Quadruple standard gap

#### Spacing Classes (Padding & Margin)

**Scale**: `0` (0), `1` (0.25rem), `2` (0.5rem), `4` (1rem), `6` (1.5rem), `8` (2rem), `10` (2.5rem), `12` (3rem), `16` (4rem)

**Padding**: `.p-*` (all), `.px-*` (horizontal), `.py-*` (vertical), `.pt-*` (top), `.pb-*` (bottom), `.ps-*` (start), `.pe-*` (end)

**Margin**: `.m-*` (all), `.mx-*` (horizontal), `.my-*` (vertical), `.mt-*` (top), `.mb-*` (bottom), `.ms-*` (start), `.me-*` (end)

### Color Utilities

### Visual Utilities

#### Opacity

- `.opacity-0` to `.opacity-100` in increments of 10

#### Border Radius

- `.rounded`: Standard border radius
- `.pill`: Pill-shaped (fully rounded)

#### Width/Height Classes

- `.w-full`, `.h-full` - Full width/height
- `.w-1/2` through `.w-5/6`, `.h-1/2` through `.h-5/6` - Fractional sizing (halves, thirds, fourths, and sixths available)

#### Aspect Ratio Classes

**Auto** `.aspect-auto` (browser default)
**Square**: `.aspect-1/1`
**Landscape**: `.aspect-4/3`, `.aspect-3/2`, `.aspect-16/9`, `.aspect-21/9`
**Portrait**: `.aspect-2/3`, `.aspect-3/4`, `.aspect-9/16`

## Components

### Badge Component

```html
<p><span class="badge">Status</span></p>
```

### Placeholder Component

```html
<div class="placeholder">Chart Area</div>
```

Styling:

- Uses a default `aspect-ratio: 4 / 3;`
  - Customize by setting `width` `height` or `aspect-ratio` properties
- Automatically stretches to fill available space
- Used for reserved areas that will be filled with charts or other content

## Usage Examples

### Title Slide

```html
<body class="col center">
  <h1>Presentation Title</h1>
  <h2 class="text-2xl opacity-70">Subtitle</h2>
  <p class="text-sm opacity-50">Author Name • Date</p>
</body>
```

### Content Slide with Sidebar

```html
<body class="col">
  <header>
    <h2 class="text-primary">Slide Title</h2>
  </header>
  <main class="fill-height row gap-lg">
    <section class="fill-width">
      <p>Main content goes here...</p>
    </section>
    <aside class="bg-muted p-4 rounded" style="min-width: 200px;">
      <div class="badge bg-primary text-primary-foreground">Important</div>
      <p class="text-sm text-muted-foreground">Sidebar content</p>
    </aside>
  </main>
</body>
```

### Two-Column Layout

```html
<body class="col">
  <h2 class="fit text-center">Comparison</h2>
  <div class="fill-height row gap-lg items-fill-width">
    <section>
      <h3>Option A</h3>
      <p>Content for option A...</p>
    </section>
    <section>
      <h3>Option B</h3>
      <p>Content for option B...</p>
    </section>
  </div>
</body>
```

### Centered Content with List

```html
<body class="col center">
  <h2>Key Points</h2>
  <ul>
    <li>First important point</li>
    <li>Second important point</li>
    <li>Third important point</li>
  </ul>
</body>
```

## Best Practices

### Layout Structure

1. **Start with body class**: Use `.col` for vertical layouts and `.row` for horizontal layouts, add `.center` for centered content
2. **Apply `.fill-width`/`.fill-height` and `.fit`**: Control which elements expand vs. maintain fixed size
3. **Maintain hierarchy**: Use appropriate heading levels (h1-h6)

### Spacing and Visual Hierarchy

1. **Consistent gaps**: Use gap classes instead of margins between flex items
2. **Padding for breathing room**: Apply padding classes to containers, not individual elements
3. **Selective margins**: Use margin classes sparingly for specific adjustments outside flex containers
4. **Directional spacing**: Use directional classes (px, py, mx, my) only when you need asymmetric spacing
5. **Typography scale**: Use utility classes for consistent font sizing
6. **Color usage**: Stick to the defined color palette for professional appearance

### Responsive Considerations

1. **Fixed dimensions**: Content must fit within 960×540px
2. **Overflow prevention**: Use `.fit` class to prevent content from growing too large
3. **Text scaling**: Use appropriate text size classes for readability
4. **White space**: Don't cram too much content on a single slide

### Performance Tips

1. **Minimal custom CSS**: Leverage utility classes instead of writing custom styles
2. **Consistent structure**: Use similar layout patterns across slides
3. **Semantic HTML**: Use appropriate HTML elements for better conversion to PowerPoint
