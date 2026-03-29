---
name: pptx
description: "Presentation creation, editing, and analysis. When Claude needs to work with presentations (.pptx files) for: (1) Creating new presentations, (2) Modifying or editing content, (3) Working with layouts, (4) Adding comments or speaker notes, or any other presentation tasks"
license: Proprietary. LICENSE.txt has complete terms
version: 1.0.0
---

# PPTX creation, editing, and analysis

## Overview

Create, edit, or analyze the contents of .pptx files when requested. A .pptx file is essentially a ZIP archive containing XML files and other resources. Different tools and workflows are available for different tasks.

## Reading and analyzing content

### Text extraction

To read just the text content of a presentation, convert the document to markdown:

```bash
# Convert document to markdown
python -m markitdown path-to-file.pptx
```

### Raw XML access

Use raw XML access for: comments, speaker notes, slide layouts, animations, design elements, and complex formatting. To access these features, unpack a presentation and read its raw XML contents.

#### Unpacking a file

`python ooxml/scripts/unpack.py <office_file> <output_dir>`

**Note**: The unpack.py script is located at `skills/pptx/ooxml/scripts/unpack.py` relative to the project root. If the script doesn't exist at this path, use `find . -name "unpack.py"` to locate it.

#### Key file structures

- `ppt/presentation.xml` - Main presentation metadata and slide references
- `ppt/slides/slide{N}.xml` - Individual slide contents (slide1.xml, slide2.xml, etc.)
- `ppt/notesSlides/notesSlide{N}.xml` - Speaker notes for each slide
- `ppt/comments/modernComment_*.xml` - Comments for specific slides
- `ppt/slideLayouts/` - Layout templates for slides
- `ppt/slideMasters/` - Master slide templates
- `ppt/theme/` - Theme and styling information
- `ppt/media/` - Images and other media files

#### Typography and color extraction

**To emulate example designs**, analyze the presentation's typography and colors first using the methods below:

1. **Read theme file**: Check `ppt/theme/theme1.xml` for colors (`<a:clrScheme>`) and fonts (`<a:fontScheme>`)
2. **Sample slide content**: Examine `ppt/slides/slide1.xml` for actual font usage (`<a:rPr>`) and colors
3. **Search for patterns**: Use grep to find color (`<a:solidFill>`, `<a:srgbClr>`) and font references across all XML files

## Creating a new PowerPoint presentation **without a template**

When creating a new PowerPoint presentation from scratch, use the **html2pptx** workflow to convert HTML slides to PowerPoint with accurate positioning.

### Workflow

1. **MANDATORY - READ ENTIRE FILE NOW**: Read [`html2pptx.md`](html2pptx.md) completely from start to finish. **NEVER set any range limits when reading this file.** Read the full file content for detailed syntax, critical formatting rules, and best practices before proceeding with presentation creation.
2. **PREREQUISITE - Install html2pptx library**:
   - Check and install if needed: `npm list -g @ant/html2pptx || npm install -g skills/pptx/html2pptx.tgz`
   - **Note**: If you see "Cannot find module '@ant/html2pptx'" error later, the package isn't installed
3. **CRITICAL**: Plan the presentation
   - Plan the shared aspects of the presentation. Describe the tone of the presentation's content and the colors and typography that should be used in the presentation.
   - Write a DETAILED outline of the presentation
   - For each slide, describe the slide's layout and contents
   - For each slide, write presenter notes (1 to 3 sentences per slide)
4. **CRITICAL**: Set CSS variables
   - In a shared `.css` file, override CSS variables to use on each slide for colors, typography, and spacing. DO NOT create classes in this file.
5. Create an HTML file for each slide with proper dimensions (e.g., 960px × 540px for 16:9)
   - Recall the outline, layout/content description, and speaker notes you wrote for this slide in Step 3. Think out loud how to best apply them to this slide.
   - Embed the contents of the shared `.css` file in a `<style>` element
   - Use `<p>`, `<h1>`-`<h6>`, `<ul>`, `<ol>` for all text content
   - **IMPORTANT:** Use CSS variables for colors, typography, and spacing
   - **IMPORTANT:** Use `row` `col` and `fit` classes for layout INSTEAD OF flexbox
   - Use `class="placeholder"` for areas where charts/tables will be added (render with gray background for visibility)
   - **CSS gradients**: Use `linear-gradient()` or `radial-gradient()` in CSS on block element backgrounds - automatically converted to PowerPoint
   - **Background images**: Use `background-image: url(...)` CSS property on block elements
   - **Block elements**: Use `<div>`, `<section>`, `<header>`, `<footer>`, `<main>`, `<article>`, `<nav>`, `<aside>` for containers with styling (all behave identically)
   - **Icons**: Use inline SVG format or reference SVG files - SVG elements are automatically converted to images in PowerPoint
   - **Text balancing**: `<h1>` and `<h2>` elements are automatically balanced. Use `data-balance` attribute on other elements to auto-balance line lengths for better typography
   - **Layout**: For slides with charts/tables/images, use either full-slide layout or two-column layout for better readability
6. Create and run a JavaScript file using the [`html2pptx`](./html2pptx) library to convert HTML slides to PowerPoint and save the presentation

   - Run with: `NODE_PATH="$(npm root -g)" node your-script.js 2>&1`
   - Use the `html2pptx` function to process each HTML file
   - Add charts and tables to placeholder areas using PptxGenJS API
   - Save the presentation using `pptx.writeFile()`

   - **⚠️ CRITICAL:** Your script MUST follow this example structure. Think aloud before writing the script to make sure that you correctly use the APIs. Do NOT call `pptx.addSlide`.

   ```javascript
   const pptxgen = require("pptxgenjs");
   const { html2pptx } = require("@ant/html2pptx");

   // Create a new pptx presentation
   const pptx = new pptxgen();
   pptx.layout = "LAYOUT_16x9"; // Must match HTML body dimensions

   // Add an HTML-only slide
   await html2pptx("slide1.html", pptx);

   // Add a HTML slide with chart placeholders
   const { slide: slide2, placeholders } = await html2pptx("slide2.html", pptx);
   slide.addChart(pptx.charts.LINE, chartData, placeholders[0]);

   // Save the presentation
   await pptx.writeFile("output.pptx");
   ```

7. **Visual validation**: Generate thumbnails and inspect for layout issues
   - Create thumbnail grid: `python scripts/thumbnail.py output.pptx workspace/thumbnails --cols 4`
   - Read and carefully examine the thumbnail image for:
     - **Text cutoff**: Text being cut off by header bars, shapes, or slide edges
     - **Text overlap**: Text overlapping with other text or shapes
     - **Positioning issues**: Content too close to slide boundaries or other elements
     - **Contrast issues**: Insufficient contrast between text and backgrounds
   - If issues found, adjust HTML margins/spacing/colors and regenerate the presentation
   - Repeat until all slides are visually correct

## Editing an existing PowerPoint presentation

To edit slides in an existing PowerPoint presentation, work with the raw Office Open XML (OOXML) format. This involves unpacking the .pptx file, editing the XML content, and repacking it.

### Workflow

1. **MANDATORY - READ ENTIRE FILE**: Read [`ooxml.md`](ooxml.md) (~500 lines) completely from start to finish. **NEVER set any range limits when reading this file.** Read the full file content for detailed guidance on OOXML structure and editing workflows before any presentation editing.
2. Unpack the presentation: `python ooxml/scripts/unpack.py <office_file> <output_dir>`
3. Edit the XML files (primarily `ppt/slides/slide{N}.xml` and related files)
4. **CRITICAL**: Validate immediately after each edit and fix any validation errors before proceeding: `python ooxml/scripts/validate.py <dir> --original <file>`
5. Pack the final presentation: `python ooxml/scripts/pack.py <input_directory> <office_file>`

## Creating a new PowerPoint presentation **using a template**

To create a presentation that follows an existing template's design, duplicate and re-arrange template slides before replacing placeholder content.

### Workflow

1. **Extract template text AND create visual thumbnail grid**:

   - Extract text: `python -m markitdown template.pptx > template-content.md`
   - Read `template-content.md`: Read the entire file to understand the contents of the template presentation. **NEVER set any range limits when reading this file.**
   - Create thumbnail grids: `python scripts/thumbnail.py template.pptx`
   - See [Creating Thumbnail Grids](#creating-thumbnail-grids) section for more details

2. **Analyze template and save inventory to a file**:

   - **Visual Analysis**: Review thumbnail grid(s) to understand slide layouts, design patterns, and visual structure
   - Create and save a template inventory file at `template-inventory.md` containing:

     ```markdown
     # Template Inventory Analysis

     **Total Slides: [count]**
     **IMPORTANT: Slides are 0-indexed (first slide = 0, last slide = count-1)**

     ## [Category Name]

     - Slide 0: [Layout code if available] - Description/purpose
     - Slide 1: [Layout code] - Description/purpose
     - Slide 2: [Layout code] - Description/purpose
       [... EVERY slide must be listed individually with its index ...]
     ```

   - **Using the thumbnail grid**: Reference the visual thumbnails to identify:
     - Layout patterns (title slides, content layouts, section dividers)
     - Image placeholder locations and counts
     - Design consistency across slide groups
     - Visual hierarchy and structure
   - This inventory file is REQUIRED for selecting appropriate templates in the next step

3. **Create presentation outline based on template inventory**:

   - Review available templates from step 2.
   - Choose an intro or title template for the first slide. This should be one of the first templates.
   - Choose safe, text-based layouts for the other slides.
   - **CRITICAL: Match layout structure to actual content**:
     - Single-column layouts: Use for unified narrative or single topic
     - Two-column layouts: Use ONLY when there are exactly 2 distinct items/concepts
     - Three-column layouts: Use ONLY when there are exactly 3 distinct items/concepts
     - Image + text layouts: Use ONLY when there are actual images to insert
     - Quote layouts: Use ONLY for actual quotes from people (with attribution), never for emphasis
     - Never use layouts with more placeholders than available content
     - With 2 items, avoid forcing them into a 3-column layout
     - With 4+ items, consider breaking into multiple slides or using a list format
   - Count actual content pieces BEFORE selecting the layout
   - Verify each placeholder in the chosen layout will be filled with meaningful content
   - Select one option representing the **best** layout for each content section.
   - Save `outline.md` with content AND template mapping that leverages available designs
   - Example template mapping:
     ```
     # Template slides to use (0-based indexing)
     # WARNING: Verify indices are within range! Template with 73 slides has indices 0-72
     # Mapping: slide numbers from outline -> template slide indices
     template_mapping = [
         0,   # Use slide 0 (Title/Cover)
         34,  # Use slide 34 (B1: Title and body)
         34,  # Use slide 34 again (duplicate for second B1)
         50,  # Use slide 50 (E1: Quote)
         54,  # Use slide 54 (F2: Closing + Text)
     ]
     ```

4. **Duplicate, reorder, and delete slides using `rearrange.py`**:

   - Use the `scripts/rearrange.py` script to create a new presentation with slides in the desired order:
     ```bash
     python scripts/rearrange.py template.pptx working.pptx 0,34,34,50,52
     ```
   - The script handles duplicating repeated slides, deleting unused slides, and reordering automatically
   - Slide indices are 0-based (first slide is 0, second is 1, etc.)
   - The same slide index can appear multiple times to duplicate that slide

5. **Extract ALL text using the `inventory.py` script**:

   - **Run inventory extraction**:
     ```bash
     python scripts/inventory.py working.pptx text-inventory.json
     ```
   - **Read text-inventory.json**: Read the entire text-inventory.json file to understand all shapes and their properties. **NEVER set any range limits when reading this file.**

   - The inventory JSON structure:

     ```json
     {
       "slide-0": {
         "shape-0": {
           "placeholder_type": "TITLE", // or null for non-placeholders
           "left": 1.5, // position in inches
           "top": 2.0,
           "width": 7.5,
           "height": 1.2,
           "paragraphs": [
             {
               "text": "Paragraph text",
               // Optional properties (only included when non-default):
               "bullet": true, // explicit bullet detected
               "level": 0, // only included when bullet is true
               "alignment": "CENTER", // CENTER, RIGHT (not LEFT)
               "space_before": 10.0, // space before paragraph in points
               "space_after": 6.0, // space after paragraph in points
               "line_spacing": 22.4, // line spacing in points
               "font_name": "Arial", // from first run
               "font_size": 14.0, // in points
               "bold": true,
               "italic": false,
               "underline": false,
               "color": "FF0000" // RGB color
             }
           ]
         }
       }
     }
     ```

   - Key features:
     - **Slides**: Named as "slide-0", "slide-1", etc.
     - **Shapes**: Ordered by visual position (top-to-bottom, left-to-right) as "shape-0", "shape-1", etc.
     - **Placeholder types**: TITLE, CENTER_TITLE, SUBTITLE, BODY, OBJECT, or null
     - **Default font size**: `default_font_size` in points extracted from layout placeholders (when available)
     - **Slide numbers are filtered**: Shapes with SLIDE_NUMBER placeholder type are automatically excluded from inventory
     - **Bullets**: When `bullet: true`, `level` is always included (even if 0)
     - **Spacing**: `space_before`, `space_after`, and `line_spacing` in points (only included when set)
     - **Colors**: `color` for RGB (e.g., "FF0000"), `theme_color` for theme colors (e.g., "DARK_1")
     - **Properties**: Only non-default values are included in the output

6. **Generate replacement text and save the data to a JSON file**
   Based on the text inventory from the previous step:

   - **CRITICAL**: First verify which shapes exist in the inventory - only reference shapes that are actually present
   - **VALIDATION**: The replace.py script validates that all shapes in the replacement JSON exist in the inventory
     - Referencing a non-existent shape produces an error showing available shapes
     - Referencing a non-existent slide produces an error indicating the slide doesn't exist
     - All validation errors are shown at once before the script exits
   - **IMPORTANT**: The replace.py script uses inventory.py internally to identify ALL text shapes
   - **AUTOMATIC CLEARING**: ALL text shapes from the inventory are cleared unless "paragraphs" are provided for them
   - Add a "paragraphs" field to shapes that need content (not "replacement_paragraphs")
   - Shapes without "paragraphs" in the replacement JSON have their text cleared automatically
   - Paragraphs with bullets are automatically left aligned. Avoid setting the `alignment` property when `"bullet": true`
   - Generate appropriate replacement content for placeholder text
   - Use shape size to determine appropriate content length
   - **CRITICAL**: Include paragraph properties from the original inventory - don't just provide text
   - **IMPORTANT**: When bullet: true, do NOT include bullet symbols (•, -, \*) in text - they're added automatically
   - **ESSENTIAL FORMATTING RULES**:
     - Headers/titles should typically have `"bold": true`
     - List items should have `"bullet": true, "level": 0` (level is required when bullet is true)
     - Preserve any alignment properties (e.g., `"alignment": "CENTER"` for centered text)
     - Include font properties when different from default (e.g., `"font_size": 14.0`, `"font_name": "Lora"`)
     - Colors: Use `"color": "FF0000"` for RGB or `"theme_color": "DARK_1"` for theme colors
     - The replacement script expects **properly formatted paragraphs**, not just text strings
     - **Overlapping shapes**: Prefer shapes with larger default_font_size or more appropriate placeholder_type
   - Save the updated inventory with replacements to `replacement-text.json`
   - **WARNING**: Different template layouts have different shape counts - always check the actual inventory before creating replacements

   Example paragraphs field showing proper formatting:

   ```json
   "paragraphs": [
     {
       "text": "New presentation title text",
       "alignment": "CENTER",
       "bold": true
     },
     {
       "text": "Section Header",
       "bold": true
     },
     {
       "text": "First bullet point without bullet symbol",
       "bullet": true,
       "level": 0
     },
     {
       "text": "Red colored text",
       "color": "FF0000"
     },
     {
       "text": "Theme colored text",
       "theme_color": "DARK_1"
     },
     {
       "text": "Regular paragraph text without special formatting"
     }
   ]
   ```

   **Shapes not listed in the replacement JSON are automatically cleared**:

   ```json
   {
     "slide-0": {
       "shape-0": {
         "paragraphs": [...] // This shape gets new text
       }
       // shape-1 and shape-2 from inventory will be cleared automatically
     }
   }
   ```

   **Common formatting patterns for presentations**:

   - Title slides: Bold text, sometimes centered
   - Section headers within slides: Bold text
   - Bullet lists: Each item needs `"bullet": true, "level": 0`
   - Body text: Usually no special properties needed
   - Quotes: May have special alignment or font properties

7. **Apply replacements using the `replace.py` script**

   ```bash
   python scripts/replace.py working.pptx replacement-text.json output.pptx
   ```

   The script will:

   - First extract the inventory of ALL text shapes using functions from inventory.py
   - Validate that all shapes in the replacement JSON exist in the inventory
   - Clear text from ALL shapes identified in the inventory
   - Apply new text only to shapes with "paragraphs" defined in the replacement JSON
   - Preserve formatting by applying paragraph properties from the JSON
   - Handle bullets, alignment, font properties, and colors automatically
   - Save the updated presentation

   Example validation errors:

   ```
   ERROR: Invalid shapes in replacement JSON:
     - Shape 'shape-99' not found on 'slide-0'. Available shapes: shape-0, shape-1, shape-4
     - Slide 'slide-999' not found in inventory
   ```

   ```
   ERROR: Replacement text made overflow worse in these shapes:
     - slide-0/shape-2: overflow worsened by 1.25" (was 0.00", now 1.25")
   ```

## Creating Thumbnail Grids

To create visual thumbnail grids of PowerPoint slides for quick analysis and reference:

```bash
python scripts/thumbnail.py template.pptx [output_prefix]
```

**Features**:

- Creates: `thumbnails.jpg` (or `thumbnails-1.jpg`, `thumbnails-2.jpg`, etc. for large decks)
- Default: 5 columns, max 30 slides per grid (5×6)
- Custom prefix: `python scripts/thumbnail.py template.pptx my-grid`
  - Note: The output prefix should include the path if you want output in a specific directory (e.g., `workspace/my-grid`)
- Adjust columns: `--cols 4` (range: 3-6, affects slides per grid)
- Grid limits: 3 cols = 12 slides/grid, 4 cols = 20, 5 cols = 30, 6 cols = 42
- Slides are zero-indexed (Slide 0, Slide 1, etc.)

**Use cases**:

- Template analysis: Quickly understand slide layouts and design patterns
- Content review: Visual overview of entire presentation
- Navigation reference: Find specific slides by their visual appearance
- Quality check: Verify all slides are properly formatted

**Examples**:

```bash
# Basic usage
python scripts/thumbnail.py presentation.pptx

# Combine options: custom name, columns
python scripts/thumbnail.py template.pptx analysis --cols 4
```

## Converting Slides to Images

To visually analyze PowerPoint slides, convert them to images using a two-step process:

1. **Convert PPTX to PDF**:

   ```bash
   soffice --headless --convert-to pdf template.pptx
   ```

2. **Convert PDF pages to JPEG images**:
   ```bash
   pdftoppm -jpeg -r 150 template.pdf slide
   ```
   This creates files like `slide-1.jpg`, `slide-2.jpg`, etc.

Options:

- `-r 150`: Sets resolution to 150 DPI (adjust for quality/size balance)
- `-jpeg`: Output JPEG format (use `-png` for PNG if preferred)
- `-f N`: First page to convert (e.g., `-f 2` starts from page 2)
- `-l N`: Last page to convert (e.g., `-l 5` stops at page 5)
- `slide`: Prefix for output files

Example for specific range:

```bash
pdftoppm -jpeg -r 150 -f 2 -l 5 template.pdf slide  # Converts only pages 2-5
```

## Code Style Guidelines

**IMPORTANT**: When generating code for PPTX operations:

- Write concise code
- Avoid verbose variable names and redundant operations
- Avoid unnecessary print statements

## Dependencies

Required dependencies (should already be installed):

- **markitdown**: `pip install "markitdown[pptx]"` (for text extraction from presentations)
- **pptxgenjs**: `npm install -g pptxgenjs` (for creating presentations via html2pptx)
- **playwright**: `npm install -g playwright` (for HTML rendering in html2pptx)
- **react-icons**: `npm install -g react-icons react react-dom` (for icons in SVG format)
- **LibreOffice**: `sudo apt-get install libreoffice` (for PDF conversion)
- **Poppler**: `sudo apt-get install poppler-utils` (for pdftoppm to convert PDF to images)
- **defusedxml**: `pip install defusedxml` (for secure XML parsing)
