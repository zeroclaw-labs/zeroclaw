# HTML to PowerPoint Guide

Convert HTML slides to PowerPoint presentations with accurate positioning using the `html2pptx.js` library.

## Table of Contents

1. [Design Principles](#design-principles)
2. [Creating HTML Slides](#creating-html-slides)
3. [Using the @ant/html2pptx Library](#using-the-html2pptx-library)
4. [Using PptxGenJS](#using-pptxgenjs)

---

## ⚠️ Prerequisites Check

Verify the @ant/html2pptx package is installed before proceeding:

```bash
# Check if installed and install if not found
npm list -g @ant/html2pptx || npm install -g skills/pptx/html2pptx.tgz
```

This command will show the package version if installed, or install it automatically if not found. No additional verification is needed.

---

### Design Principles

**CRITICAL**: Analyze the content and choose appropriate design elements before creating presentations:

1. **Consider the subject matter**: What is this presentation about? What tone, industry, or mood does it suggest?
2. **Check for branding**: If the user mentions a company/organization, consider their brand colors and identity
3. **Match palette to content**: Select colors that reflect the subject
4. **State your approach**: Explain your design choices before writing code

**Requirements**:

- ✅ State your content-informed design approach BEFORE writing code
- ✅ Use web-safe fonts only: Arial, Helvetica, Times New Roman, Georgia, Courier New, Verdana, Tahoma, Trebuchet MS, Impact
- ✅ Create clear visual hierarchy through size, weight, and color
- ✅ Ensure readability: strong contrast, appropriately sized text, clean alignment
- ✅ Be consistent: repeat patterns, spacing, and visual language across slides

#### Typography and content design

This is a presentation, not a report. Keep slides brief!

- Paragraphs should be 1 sentence, _maybe_ 2.
- You should restrict yourself to 3-5 bullet points per list.
- Cards should support short statements/fragments. Maybe a complete sentence if it's short.

We've found without instruction, agents tend to write 2-3x more text than optimal, so whatever you think is short, it's probably not short enough!

For visual heirarchy reasons, you should generally not have more than 2 text sizes per slide, 3 or 4 for complex slides with a lot of components and charts. If you need additional distinction, consider using opacity.

#### Color Palette Selection

**Choosing colors creatively**:

- **Think beyond defaults**: What colors genuinely match this specific topic? Avoid autopilot choices.
- **Consider multiple angles**: Topic, industry, mood, energy level, target audience, brand identity (if mentioned)
- **Be adventurous**: Try unexpected combinations - a healthcare presentation doesn't have to be green, finance doesn't have to be navy
- **Build your palette**: Pick 3-5 colors that work together (dominant colors + supporting tones + accent)
- **Ensure contrast**: Text must be clearly readable on backgrounds

**Example color palettes** (use these to spark creativity - choose one, adapt it, or create your own):

1. **Classic Blue**: Deep navy (#1C2833), slate gray (#2E4053), silver (#AAB7B8), off-white (#F4F6F6)
2. **Teal & Coral**: Teal (#5EA8A7), deep teal (#277884), coral (#FE4447), white (#FFFFFF)
3. **Bold Red**: Red (#C0392B), bright red (#E74C3C), orange (#F39C12), yellow (#F1C40F), green (#2ECC71)
4. **Warm Blush**: Mauve (#A49393), blush (#EED6D3), rose (#E8B4B8), cream (#FAF7F2)
5. **Burgundy Luxury**: Burgundy (#5D1D2E), crimson (#951233), rust (#C15937), gold (#997929)
6. **Deep Purple & Emerald**: Purple (#B165FB), dark blue (#181B24), emerald (#40695B), white (#FFFFFF)
7. **Cream & Forest Green**: Cream (#FFE1C7), forest green (#40695B), white (#FCFCFC)
8. **Pink & Purple**: Pink (#F8275B), coral (#FF574A), rose (#FF737D), purple (#3D2F68)
9. **Lime & Plum**: Lime (#C5DE82), plum (#7C3A5F), coral (#FD8C6E), blue-gray (#98ACB5)
10. **Black & Gold**: Gold (#BF9A4A), black (#000000), cream (#F4F6F6)
11. **Sage & Terracotta**: Sage (#87A96B), terracotta (#E07A5F), cream (#F4F1DE), charcoal (#2C2C2C)
12. **Charcoal & Red**: Charcoal (#292929), red (#E33737), light gray (#CCCBCB)
13. **Vibrant Orange**: Orange (#F96D00), light gray (#F2F2F2), charcoal (#222831)
14. **Forest Green**: Black (#191A19), green (#4E9F3D), dark green (#1E5128), white (#FFFFFF)
15. **Retro Rainbow**: Purple (#722880), pink (#D72D51), orange (#EB5C18), amber (#F08800), gold (#DEB600)
16. **Vintage Earthy**: Mustard (#E3B448), sage (#CBD18F), forest green (#3A6B35), cream (#F4F1DE)
17. **Coastal Rose**: Old rose (#AD7670), beaver (#B49886), eggshell (#F3ECDC), ash gray (#BFD5BE)
18. **Orange & Turquoise**: Light orange (#FC993E), grayish turquoise (#667C6F), white (#FCFCFC)

#### Maintaining visual interest

Make use of icons, image placeholders, and charts to keep slides engaging.

#### Visual Details Options

**Geometric Patterns**:

- Diagonal section dividers instead of horizontal
- Asymmetric column widths (30/70, 40/60, 25/75)
- Rotated text headers at 90° or 270°
- Circular/hexagonal frames for images
- Triangular accent shapes in corners
- Overlapping shapes for depth

**Border & Frame Treatments**:

- Thick single-color borders (10-20px) on one side only
- Double-line borders with contrasting colors
- Corner brackets instead of full frames
- L-shaped borders (top+left or bottom+right)
- Underline accents beneath headers (4-6px thick)

**Typography Treatments**:

- Extreme size contrast (72px headlines vs 12px body)
- All-caps headers with wide letter spacing
- Numbered sections in oversized display type
- Monospace (Courier New) for data/stats/technical content
- Condensed fonts (Arial Narrow) for dense information
- Outlined text for emphasis

**Chart & Data Styling**:

- Monochrome charts with single accent color for key data
- Horizontal bar charts instead of vertical
- Dot plots instead of bar charts
- Minimal gridlines or none at all
- Data labels directly on elements (no legends)
- Oversized numbers for key metrics

**Layout Innovations**:

- Full-bleed images with text overlays
- Sidebar column (20-30% width) for navigation/context
- Modular grid systems (3×3, 4×4 blocks)
- Z-pattern or F-pattern content flow
- Floating text boxes over colored shapes
- Magazine-style multi-column layouts

**Background Treatments**:

- Solid color blocks occupying 40-60% of slide
- Gradient fills (vertical or diagonal only)
- Split backgrounds (two colors, diagonal or vertical)
- Edge-to-edge color bands
- Negative space as a design element

### Layout Tips

**To create slides with charts or tables:**

- **Two-column layout (PREFERRED)**: Use a header spanning the full width, then two columns below - text/bullets in one column and the featured content in the other. This provides better balance and makes charts/tables more readable. Use unequal column widths (e.g., 40%/60% split) to optimize space for each content type.
- **Full-slide layout**: Let the featured content (chart/table) take up the entire slide for maximum impact and readability
- **NEVER vertically stack**: Do not place charts/tables below text in a single column - this causes poor readability and layout issues

## Creating HTML Slides

Every HTML slide must include proper body dimensions:

- **16:9** (automatically applied): `width: 960px; height: 540px`
- **4:3**: `width: 960px; height: 720px`
- **16:10**: `width: 960px; height: 600px`

### How to write CSS

**MANDATORY - READ ENTIRE FILE**: Read [`css.md`](css.md) (~400 lines) completely from start to finish. **NEVER set any range limits when reading this file.** Read the full file content for detailed guidance on CSS structure before writing any HTML.

Slides are automatically provided with a global stylesheet which is injected when the HTML is rendered. Guidelines for styles:

- CRITICAL: REFRAIN FROM DEFINING YOUR OWN TYPE SIZES AND COLORS unless you are explicity "hitting the eject button." Use variables defined in the global stylesheet whenever possible.
- Override these CSS variables (using the `:root` selector) to customize the look and feel of your slides
- Use the classes from [`css.md`](css.md) when creating your slides. Reference the examples provided in that guide.

### Supported Elements

#### Block Elements

- `<div>`, `<section>`, `<header>`, `<footer>`, `<main>`, `<article>`, `<nav>`, `<aside>` - Container elements with bg/border support (supports gradients and background images)

#### Text Elements

- `<p>` - Paragraphs with styling
- `<h1>`-`<h6>` - Headings with styling

#### Lists

- `<ul>`, `<ol>` - Lists (never use manual bullets •, -, \*)

#### Inline Formatting

- `<b>`, `<strong>` - Bold text
- `<i>`, `<em>` - Italic text
- `<u>` - Underlined text
- `<span>` - Inline formatting with CSS styles (bold, italic, underline, color)
- `<br>` - Line breaks

#### Media

- `<img>` - Images

#### Special Features

- `class="placeholder"` - Reserved space for charts (returns `{ id, x, y, w, h }`)
  - Automatically styled with muted background and dashed border
  - Stretches to fill available container space
  - Provides visual indication during development
- `data-balance` attribute - Auto-balance text line lengths for better typography. `<h1>` and `<h2>` elements are automatically balanced without needing the `data-balance` attribute.

### Critical Text Rules

**IMPORTANT**: These rules must be followed to safely convert HTML to PowerPoint.

**ALL text MUST be inside `<p>`, `<h1>`-`<h6>`, `<ul>`, or `<ol>` tags:**

- ✅ Correct: `<div><p>Text here</p></div>`
- ❌ Wrong: `<div>Text here</div>` - **Text will NOT appear in PowerPoint**
- ❌ Wrong: `<span>Text</span>` - **Text will NOT appear in PowerPoint**
- Text in `<div>` or `<span>` without a text tag is silently ignored

**NEVER use manual bullet symbols (•, -, \*, etc.)** - Use `<ul>` or `<ol>` lists instead

**Use `row` and `col` classes INSTEAD of flexbox:**

- ✅ Correct: `<div class="row"><p>Text here</p></div>`
- ❌ Wrong: `<div style="display: flex;"><p>Text here</p></div>`

**ONLY use web-safe fonts that are universally available:**

- ✅ Web-safe fonts: `Arial`, `Helvetica`, `Times New Roman`, `Georgia`, `Courier New`, `Verdana`, `Tahoma`, `Trebuchet MS`, `Impact`, `Comic Sans MS`
- ❌ Wrong: `'Segoe UI'`, `'SF Pro'`, `'Roboto'`, custom fonts - **May cause rendering issues**

### Shape Styling (block elements only)

**IMPORTANT: Backgrounds, borders, and shadows only work on block elements, NOT on text elements (`<p>`, `<h1>`-`<h6>`, `<ul>`, `<ol>`)**

- **Backgrounds**: CSS `background` or `background-color` or `background-image`
  - `background: var(--color-surface);`
  - `background: linear-gradient(135deg, var(--color-primary-light) 0%, var(--color-primary-dark) 100%);`
  - `background: radial-gradient(circle, var(--color-accent-light) 0%, var(--color-accent-dark) 100%);`
  - `background: url(path/to/image.png)`
- **Borders**
  - Supports uniform borders: `border: 1px solid var(--color-border)`
  - Supports partial borders: `border-left`, `border-right`, `border-top`, `border-bottom`
- **Border radius**
  - `rounded` CSS class applies the default border-radius
  - `pill` CSS class applies maximum border-radius to create pill-shaped elements
    - When height and width are equal, this creates a circle
- **Box shadows**
  - Supports outer shadows only
    - PowerPoint does not support inset shadows
  - `box-shadow: 2px 2px 8px rgba(0, 0, 0, 0.3);`

### Icons

Icons can be included using either inline SVG or SVG files, which are automatically converted to images in PowerPoint.

#### How to use react-icons

```javascript
const React = require("react");
const ReactDOMServer = require("react-dom/server");
const { FaHome } = require("react-icons/fa");

// Generate SVG string from react-icon
function renderIconSvg(IconComponent, color, size = "48") {
  return ReactDOMServer.renderToStaticMarkup(
    React.createElement(IconComponent, { color: color, size: size })
  );
}

// Get SVG markup
const homeIconSvg = renderIconSvg(FaHome, "#4472c4", "48");

// Use in HTML template (inline SVG)
// <div style="width: 48px; height: 48px;">${homeIconSvg}</div>
```

### Example Slide HTML

```html
<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Slide with title, context, and full bleed placeholder</title>
    <style>
      /* Shared CSS variable overrides */
      :root {
        --color-primary: #00a4fc;
        --color-primary-foreground: #ffffff;
      }
    </style>
  </head>
  <body class="row items-fill-width gap-lg">
    <div class="p-8 pe-0">
      <h1>Slide title</h1>
      <p class="text-2xl text-muted-foreground">Subtitle or context</p>
    </div>
    <div class="placeholder w-3/5 fit"></div>
  </body>
</html>
```

## Using the @ant/html2pptx Library

### Installation & Setup

**Important**: Install the @ant/html2pptx package globally before using this library. See the **Prerequisites Check** section at the top of this document.

**When running scripts, always set NODE_PATH:**

```sh
NODE_PATH="$(npm root -g)" node your-script.js 2>&1
```

**If you get "Cannot find module" errors**, see the Prerequisites Check section or verify that NODE_PATH is correctly pointing to the global node_modules directory.

### Dependencies

These libraries have been globally installed and are available to use:

- `pptxgenjs`
- `playwright`

### ⚠️ IMPORTANT: How To Use html2pptx

Common errors:

- DO NOT call `pptx.addSlide()` directly, `html2pptx` creates a slide for you
- `html2pptx` accepts an `htmlFilePath` and a `pptx` presentation object
  - If you pass the wrong arguments, your script will throw errors or time out

**Your script MUST follow the following example.**

```javascript
const pptxgen = require("pptxgenjs");
const { html2pptx } = require("@ant/html2pptx");

// Create a new pptx presentation
const pptx = new pptxgen();
pptx.layout = "LAYOUT_16x9"; // Must match HTML body dimensions

// Add an HTML-only slide
await html2pptx("slide1.html", pptx);

// Add a slide with a chart placeholder
const { slide, placeholders } = await html2pptx("slide2.html", pptx);
slide.addChart(pptx.charts.LINE, chartData, placeholders[0]);

// Save the presentation
await pptx.writeFile("output.pptx");
```

### API Reference

#### Function Signature

```javascript
await html2pptx(htmlFilePath, pptxPresentation, options);
```

#### Parameters

- `htmlFilePath` (string): Path to HTML file (absolute or relative)
- `pptxPresentation` (pptxgen): PptxGenJS presentation instance with layout already set
- `options` (object, optional):
  - `tmpDir` (string): Temporary directory for generated files (default: `process.env.TMPDIR || '/tmp'`)

#### Returns

```javascript
{
    slide: pptxgenSlide,           // The created/updated slide
    placeholders: [                 // Array of placeholder positions
        { id: string, x: number, y: number, w: number, h: number },
        ...
    ]
}
```

### Validation

The library automatically validates and collects all errors before throwing:

1. **HTML dimensions must match presentation layout** - Reports dimension mismatches
2. **Content must not overflow body** - Reports overflow with exact measurements
3. **Text element styling** - Reports backgrounds/borders/shadows on text elements (only allowed on block elements)

**All validation errors are collected and reported together** in a single error message, allowing you to fix all issues at once instead of one at a time.

### Working with Placeholders

```javascript
const { slide, placeholders } = await html2pptx("slide.html", pptx);

// Use first placeholder
slide.addChart(pptx.charts.BAR, data, placeholders[0]);

// Find by ID
const chartArea = placeholders.find((p) => p.id === "chart-area");
slide.addChart(pptx.charts.LINE, data, chartArea);
```

### Complete Example

```javascript
const pptxgen = require("pptxgenjs");
const { html2pptx } = require("@ant/html2pptx");

async function createPresentation() {
  const pptx = new pptxgen();
  pptx.layout = "LAYOUT_16x9";
  pptx.author = "Your Name";
  pptx.title = "My Presentation";

  // Slide 1: Title
  const { slide: slide1 } = await html2pptx("slides/title.html", pptx);

  // Slide 2: Content with chart
  const { slide: slide2, placeholders } = await html2pptx(
    "slides/data.html",
    pptx
  );

  const chartData = [
    {
      name: "Sales",
      labels: ["Q1", "Q2", "Q3", "Q4"],
      values: [4500, 5500, 6200, 7100],
    },
  ];

  slide2.addChart(pptx.charts.BAR, chartData, {
    ...placeholders[0],
    showTitle: true,
    title: "Quarterly Sales",
    showCatAxisTitle: true,
    catAxisTitle: "Quarter",
    showValAxisTitle: true,
    valAxisTitle: "Sales ($000s)",
  });

  // Save
  await pptx.writeFile({ fileName: "presentation.pptx" });
  console.log("Presentation created successfully!");
}

createPresentation().catch(console.error);
```

**Run with:**

```sh
NODE_PATH="$(npm root -g)" node create-presentation.js
```

## Using PptxGenJS

After converting HTML to slides with `html2pptx`, you'll use PptxGenJS to add dynamic content like charts, images, and additional elements.

### ⚠️ Critical Rules

#### Colors

- **NEVER use `#` prefix** with hex colors in PptxGenJS - causes file corruption
- ✅ Correct: `color: "FF0000"`, `fill: { color: "0066CC" }`
- ❌ Wrong: `color: "#FF0000"` (breaks document)

### Adding Images

Always calculate aspect ratios from actual image dimensions:

```javascript
// Get image dimensions: identify image.png | grep -o '[0-9]* x [0-9]*'
const imgWidth = 1860,
  imgHeight = 1519; // From actual file
const aspectRatio = imgWidth / imgHeight;

const h = 3; // Max height
const w = h * aspectRatio;
const x = (10 - w) / 2; // Center on 16:9 slide

slide.addImage({ path: "chart.png", x, y: 1.5, w, h });
```

### Adding Text

```javascript
// Rich text with formatting
slide.addText(
  [
    { text: "Bold ", options: { bold: true } },
    { text: "Italic ", options: { italic: true } },
    { text: "Normal" },
  ],
  {
    x: 1,
    y: 2,
    w: 8,
    h: 1,
  }
);
```

### Adding Shapes

```javascript
// Rectangle
slide.addShape(pptx.shapes.RECTANGLE, {
  x: 1,
  y: 1,
  w: 3,
  h: 2,
  fill: { color: "4472C4" },
  line: { color: "000000", width: 2 },
});

// Circle
slide.addShape(pptx.shapes.OVAL, {
  x: 5,
  y: 1,
  w: 2,
  h: 2,
  fill: { color: "ED7D31" },
});

// Rounded rectangle
slide.addShape(pptx.shapes.ROUNDED_RECTANGLE, {
  x: 1,
  y: 4,
  w: 3,
  h: 1.5,
  fill: { color: "70AD47" },
  rectRadius: 0.2,
});
```

### Adding Charts

**Required for most charts:** Axis labels using `catAxisTitle` (category) and `valAxisTitle` (value).

**Chart Data Format:**

- Use **single series with all labels** for simple bar/line charts
- Each series creates a separate legend entry
- Labels array defines X-axis values

**Time Series Data - Choose Correct Granularity:**

- **< 30 days**: Use daily grouping (e.g., "10-01", "10-02") - avoid monthly aggregation that creates single-point charts
- **30-365 days**: Use monthly grouping (e.g., "2024-01", "2024-02")
- **> 365 days**: Use yearly grouping (e.g., "2023", "2024")
- **Validate**: Charts with only 1 data point likely indicate incorrect aggregation for the time period

```javascript
const { slide, placeholders } = await html2pptx("slide.html", pptx);

// CORRECT: Single series with all labels
slide.addChart(
  pptx.charts.BAR,
  [
    {
      name: "Sales 2024",
      labels: ["Q1", "Q2", "Q3", "Q4"],
      values: [4500, 5500, 6200, 7100],
    },
  ],
  {
    ...placeholders[0], // Use placeholder position
    barDir: "col", // 'col' = vertical bars, 'bar' = horizontal
    showTitle: true,
    title: "Quarterly Sales",
    showLegend: false, // No legend needed for single series
    // Required axis labels
    showCatAxisTitle: true,
    catAxisTitle: "Quarter",
    showValAxisTitle: true,
    valAxisTitle: "Sales ($000s)",
    // Optional: Control scaling (adjust min based on data range for better visualization)
    valAxisMaxVal: 8000,
    valAxisMinVal: 0, // Use 0 for counts/amounts; for clustered data (e.g., 4500-7100), consider starting closer to min value
    valAxisMajorUnit: 2000, // Control y-axis label spacing to prevent crowding
    catAxisLabelRotate: 45, // Rotate labels if crowded
    dataLabelPosition: "outEnd",
    dataLabelColor: "000000",
    // Use single color for single-series charts
    chartColors: ["4472C4"], // All bars same color
  }
);
```

#### Scatter Chart

**IMPORTANT**: Scatter chart data format is unusual - first series contains X-axis values, subsequent series contain Y-values:

```javascript
// Prepare data
const data1 = [
  { x: 10, y: 20 },
  { x: 15, y: 25 },
  { x: 20, y: 30 },
];
const data2 = [
  { x: 12, y: 18 },
  { x: 18, y: 22 },
];

const allXValues = [...data1.map((d) => d.x), ...data2.map((d) => d.x)];

slide.addChart(
  pptx.charts.SCATTER,
  [
    { name: "X-Axis", values: allXValues }, // First series = X values
    { name: "Series 1", values: data1.map((d) => d.y) }, // Y values only
    { name: "Series 2", values: data2.map((d) => d.y) }, // Y values only
  ],
  {
    x: 1,
    y: 1,
    w: 8,
    h: 4,
    lineSize: 0, // 0 = no connecting lines
    lineDataSymbol: "circle",
    lineDataSymbolSize: 6,
    showCatAxisTitle: true,
    catAxisTitle: "X Axis",
    showValAxisTitle: true,
    valAxisTitle: "Y Axis",
    chartColors: ["4472C4", "ED7D31"],
  }
);
```

#### Line Chart

```javascript
slide.addChart(
  pptx.charts.LINE,
  [
    {
      name: "Temperature",
      labels: ["Jan", "Feb", "Mar", "Apr"],
      values: [32, 35, 42, 55],
    },
  ],
  {
    x: 1,
    y: 1,
    w: 8,
    h: 4,
    lineSize: 4,
    lineSmooth: true,
    // Required axis labels
    showCatAxisTitle: true,
    catAxisTitle: "Month",
    showValAxisTitle: true,
    valAxisTitle: "Temperature (°F)",
    // Optional: Y-axis range (set min based on data range for better visualization)
    valAxisMinVal: 0, // For ranges starting at 0 (counts, percentages, etc.)
    valAxisMaxVal: 60,
    valAxisMajorUnit: 20, // Control y-axis label spacing to prevent crowding (e.g., 10, 20, 25)
    // valAxisMinVal: 30,  // PREFERRED: For data clustered in a range (e.g., 32-55 or ratings 3-5), start axis closer to min value to show variation
    // Optional: Chart colors
    chartColors: ["4472C4", "ED7D31", "A5A5A5"],
  }
);
```

#### Pie Chart (No Axis Labels Required)

**CRITICAL**: Pie charts require a **single data series** with all categories in the `labels` array and corresponding values in the `values` array.

```javascript
slide.addChart(
  pptx.charts.PIE,
  [
    {
      name: "Market Share",
      labels: ["Product A", "Product B", "Other"], // All categories in one array
      values: [35, 45, 20], // All values in one array
    },
  ],
  {
    x: 2,
    y: 1,
    w: 6,
    h: 4,
    showPercent: true,
    showLegend: true,
    legendPos: "r", // right
    chartColors: ["4472C4", "ED7D31", "A5A5A5"],
  }
);
```

#### Multiple Data Series

```javascript
slide.addChart(
  pptx.charts.LINE,
  [
    {
      name: "Product A",
      labels: ["Q1", "Q2", "Q3", "Q4"],
      values: [10, 20, 30, 40],
    },
    {
      name: "Product B",
      labels: ["Q1", "Q2", "Q3", "Q4"],
      values: [15, 25, 20, 35],
    },
  ],
  {
    x: 1,
    y: 1,
    w: 8,
    h: 4,
    showCatAxisTitle: true,
    catAxisTitle: "Quarter",
    showValAxisTitle: true,
    valAxisTitle: "Revenue ($M)",
  }
);
```

### Chart Colors

**CRITICAL**: Use hex colors **without** the `#` prefix - including `#` causes file corruption.

**Align chart colors with your chosen design palette**, ensuring sufficient contrast and distinctiveness for data visualization. Adjust colors for:

- Strong contrast between adjacent series
- Readability against slide backgrounds
- Accessibility (avoid red-green only combinations)

```javascript
// Example: Ocean palette-inspired chart colors (adjusted for contrast)
const chartColors = ["16A085", "FF6B9D", "2C3E50", "F39C12", "9B59B6"];

// Single-series chart: Use one color for all bars/points
slide.addChart(
  pptx.charts.BAR,
  [
    {
      name: "Sales",
      labels: ["Q1", "Q2", "Q3", "Q4"],
      values: [4500, 5500, 6200, 7100],
    },
  ],
  {
    ...placeholders[0],
    chartColors: ["16A085"], // All bars same color
    showLegend: false,
  }
);

// Multi-series chart: Each series gets a different color
slide.addChart(
  pptx.charts.LINE,
  [
    { name: "Product A", labels: ["Q1", "Q2", "Q3"], values: [10, 20, 30] },
    { name: "Product B", labels: ["Q1", "Q2", "Q3"], values: [15, 25, 20] },
  ],
  {
    ...placeholders[0],
    chartColors: ["16A085", "FF6B9D"], // One color per series
  }
);
```

### Adding Tables

Tables can be added with basic or advanced formatting:

#### Basic Table

```javascript
slide.addTable(
  [
    ["Header 1", "Header 2", "Header 3"],
    ["Row 1, Col 1", "Row 1, Col 2", "Row 1, Col 3"],
    ["Row 2, Col 1", "Row 2, Col 2", "Row 2, Col 3"],
  ],
  {
    x: 0.5,
    y: 1,
    w: 9,
    h: 3,
    border: { pt: 1, color: "999999" },
    fill: { color: "F1F1F1" },
  }
);
```

#### Table with Custom Formatting

```javascript
const tableData = [
  // Header row with custom styling
  [
    {
      text: "Product",
      options: { fill: { color: "4472C4" }, color: "FFFFFF", bold: true },
    },
    {
      text: "Revenue",
      options: { fill: { color: "4472C4" }, color: "FFFFFF", bold: true },
    },
    {
      text: "Growth",
      options: { fill: { color: "4472C4" }, color: "FFFFFF", bold: true },
    },
  ],
  // Data rows
  ["Product A", "$50M", "+15%"],
  ["Product B", "$35M", "+22%"],
  ["Product C", "$28M", "+8%"],
];

slide.addTable(tableData, {
  x: 1,
  y: 1.5,
  w: 8,
  h: 3,
  colW: [3, 2.5, 2.5], // Column widths
  rowH: [0.5, 0.6, 0.6, 0.6], // Row heights
  border: { pt: 1, color: "CCCCCC" },
  align: "center",
  valign: "middle",
  fontSize: 14,
});
```

#### Table with Merged Cells

```javascript
const mergedTableData = [
  [
    {
      text: "Q1 Results",
      options: {
        colspan: 3,
        fill: { color: "4472C4" },
        color: "FFFFFF",
        bold: true,
      },
    },
  ],
  ["Product", "Sales", "Market Share"],
  ["Product A", "$25M", "35%"],
  ["Product B", "$18M", "25%"],
];

slide.addTable(mergedTableData, {
  x: 1,
  y: 1,
  w: 8,
  h: 2.5,
  colW: [3, 2.5, 2.5],
  border: { pt: 1, color: "DDDDDD" },
});
```

### Table Options

Common table options:

- `x, y, w, h` - Position and size
- `colW` - Array of column widths (in inches)
- `rowH` - Array of row heights (in inches)
- `border` - Border style: `{ pt: 1, color: "999999" }`
- `fill` - Background color (no # prefix)
- `align` - Text alignment: "left", "center", "right"
- `valign` - Vertical alignment: "top", "middle", "bottom"
- `fontSize` - Text size
- `autoPage` - Auto-create new slides if content overflows
