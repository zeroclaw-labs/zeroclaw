# DOCX Library Tutorial

Generate .docx files with JavaScript/TypeScript.

**Important: Read this entire document before starting.** Critical formatting rules and common pitfalls are covered throughout - skipping sections may result in corrupted files or rendering issues.

## Setup
Assumes docx is already installed globally
If not installed: `npm install -g docx`

```javascript
const { Document, Packer, Paragraph, TextRun, Table, TableRow, TableCell, ImageRun, Media, 
        Header, Footer, AlignmentType, PageOrientation, LevelFormat, ExternalHyperlink, 
        InternalHyperlink, TableOfContents, HeadingLevel, BorderStyle, WidthType, TabStopType, 
        TabStopPosition, UnderlineType, ShadingType, VerticalAlign, SymbolRun, PageNumber,
        FootnoteReferenceRun, Footnote, PageBreak } = require('docx');

// Create & Save
const doc = new Document({ sections: [{ children: [/* content */] }] });
Packer.toBuffer(doc).then(buffer => fs.writeFileSync("doc.docx", buffer)); // Node.js
Packer.toBlob(doc).then(blob => { /* download logic */ }); // Browser
```

## Text & Formatting
```javascript
// IMPORTANT: Never use \n for line breaks - always use separate Paragraph elements
// ❌ WRONG: new TextRun("Line 1\nLine 2")
// ✅ CORRECT: new Paragraph({ children: [new TextRun("Line 1")] }), new Paragraph({ children: [new TextRun("Line 2")] })

// Basic text with all formatting options
new Paragraph({
  alignment: AlignmentType.CENTER,
  spacing: { before: 200, after: 200 },
  indent: { left: 720, right: 720 },
  children: [
    new TextRun({ text: "Bold", bold: true }),
    new TextRun({ text: "Italic", italics: true }),
    new TextRun({ text: "Underlined", underline: { type: UnderlineType.DOUBLE, color: "FF0000" } }),
    new TextRun({ text: "Colored", color: "FF0000", size: 28, font: "Arial" }), // Arial default
    new TextRun({ text: "Highlighted", highlight: "yellow" }),
    new TextRun({ text: "Strikethrough", strike: true }),
    new TextRun({ text: "x2", superScript: true }),
    new TextRun({ text: "H2O", subScript: true }),
    new TextRun({ text: "SMALL CAPS", smallCaps: true }),
    new SymbolRun({ char: "2022", font: "Symbol" }), // Bullet •
    new SymbolRun({ char: "00A9", font: "Arial" })   // Copyright © - Arial for symbols
  ]
})
```

## Styles & Professional Formatting

```javascript
const doc = new Document({
  styles: {
    default: { document: { run: { font: "Arial", size: 24 } } }, // 12pt default
    paragraphStyles: [
      // Document title style - override built-in Title style
      { id: "Title", name: "Title", basedOn: "Normal",
        run: { size: 56, bold: true, color: "000000", font: "Arial" },
        paragraph: { spacing: { before: 240, after: 120 }, alignment: AlignmentType.CENTER } },
      // IMPORTANT: Override built-in heading styles by using their exact IDs
      { id: "Heading1", name: "Heading 1", basedOn: "Normal", next: "Normal", quickFormat: true,
        run: { size: 32, bold: true, color: "000000", font: "Arial" }, // 16pt
        paragraph: { spacing: { before: 240, after: 240 }, outlineLevel: 0 } }, // Required for TOC
      { id: "Heading2", name: "Heading 2", basedOn: "Normal", next: "Normal", quickFormat: true,
        run: { size: 28, bold: true, color: "000000", font: "Arial" }, // 14pt
        paragraph: { spacing: { before: 180, after: 180 }, outlineLevel: 1 } },
      // Custom styles use your own IDs
      { id: "myStyle", name: "My Style", basedOn: "Normal",
        run: { size: 28, bold: true, color: "000000" },
        paragraph: { spacing: { after: 120 }, alignment: AlignmentType.CENTER } }
    ],
    characterStyles: [{ id: "myCharStyle", name: "My Char Style",
      run: { color: "FF0000", bold: true, underline: { type: UnderlineType.SINGLE } } }]
  },
  sections: [{
    properties: { page: { margin: { top: 1440, right: 1440, bottom: 1440, left: 1440 } } },
    children: [
      new Paragraph({ heading: HeadingLevel.TITLE, children: [new TextRun("Document Title")] }), // Uses overridden Title style
      new Paragraph({ heading: HeadingLevel.HEADING_1, children: [new TextRun("Heading 1")] }), // Uses overridden Heading1 style
      new Paragraph({ style: "myStyle", children: [new TextRun("Custom paragraph style")] }),
      new Paragraph({ children: [
        new TextRun("Normal with "),
        new TextRun({ text: "custom char style", style: "myCharStyle" })
      ]})
    ]
  }]
});
```

## Korean Legal Document Formatting (한국 법률문서 서식)

**중요: 법률문서 작성시 반드시 아래 서식 규칙을 따를 것**

### Default Settings (기본 설정)
1. **본문 양쪽정렬 (Justified)**: 모든 본문 텍스트는 `AlignmentType.JUSTIFIED` 사용
2. **제목 가운데정렬 + 볼드**: 문서 제목은 반드시 `AlignmentType.CENTER` + `bold: true`
3. **들여쓰기 금지**: `indent` 속성 사용하지 말 것 (들여쓰기, 내어쓰기 모두 금지)
4. **폰트**: 맑은 고딕 (Malgun Gothic) 사용 권장

```javascript
const { Document, Packer, Paragraph, TextRun, AlignmentType, LevelFormat } = require('docx');
const fs = require('fs');

const doc = new Document({
  styles: {
    default: {
      document: {
        run: { font: "맑은 고딕", size: 24 },  // 12pt 기본
        paragraph: { alignment: AlignmentType.JUSTIFIED }  // 본문 양쪽정렬 기본값
      }
    },
    paragraphStyles: [
      // 문서 제목 - 가운데 정렬, 볼드, 18pt
      { id: "Title", name: "Title", basedOn: "Normal",
        run: { size: 36, bold: true, font: "맑은 고딕" },
        paragraph: { spacing: { before: 400, after: 400 }, alignment: AlignmentType.CENTER } },
      // 소제목 1단계 - 볼드, 14pt
      { id: "Heading1", name: "Heading 1", basedOn: "Normal", next: "Normal", quickFormat: true,
        run: { size: 28, bold: true, font: "맑은 고딕" },
        paragraph: { spacing: { before: 240, after: 120 }, alignment: AlignmentType.JUSTIFIED } },
      // 소제목 2단계 - 볼드, 12pt
      { id: "Heading2", name: "Heading 2", basedOn: "Normal", next: "Normal", quickFormat: true,
        run: { size: 24, bold: true, font: "맑은 고딕" },
        paragraph: { spacing: { before: 200, after: 100 }, alignment: AlignmentType.JUSTIFIED } },
      // 본문 스타일 - 양쪽정렬
      { id: "BodyText", name: "Body Text", basedOn: "Normal",
        run: { size: 24, font: "맑은 고딕" },
        paragraph: { spacing: { after: 120, line: 360 }, alignment: AlignmentType.JUSTIFIED } }
    ]
  },
  // 한국 법률문서 번호 체계
  numbering: {
    config: [
      // 1단계: 1. 2. 3. (아라비아 숫자)
      { reference: "korean-legal-level1",
        levels: [{ level: 0, format: LevelFormat.DECIMAL, text: "%1.", alignment: AlignmentType.LEFT,
          style: { paragraph: { spacing: { before: 120 } } } }] },
      // 2단계: 가. 나. 다. (한글 가나다)
      { reference: "korean-legal-level2",
        levels: [{ level: 0, format: LevelFormat.KOREAN_LEGAL, text: "%1.", alignment: AlignmentType.LEFT,
          style: { paragraph: { spacing: { before: 80 } } } }] },
      // 3단계: (1) (2) (3) (괄호 숫자)
      { reference: "korean-legal-level3",
        levels: [{ level: 0, format: LevelFormat.DECIMAL, text: "(%1)", alignment: AlignmentType.LEFT,
          style: { paragraph: { spacing: { before: 60 } } } }] },
      // 4단계: (가) (나) (다) (괄호 한글)
      { reference: "korean-legal-level4",
        levels: [{ level: 0, format: LevelFormat.KOREAN_LEGAL, text: "(%1)", alignment: AlignmentType.LEFT,
          style: { paragraph: { spacing: { before: 40 } } } }] }
    ]
  },
  sections: [{
    properties: { page: { margin: { top: 1440, right: 1440, bottom: 1440, left: 1440 } } },
    children: [
      // 제목 - 가운데 정렬, 볼드
      new Paragraph({
        alignment: AlignmentType.CENTER,
        children: [new TextRun({ text: "항 소 이 유 서", bold: true, size: 36, font: "맑은 고딕" })]
      }),
      // 본문 - 양쪽 정렬
      new Paragraph({
        alignment: AlignmentType.JUSTIFIED,
        children: [new TextRun({ text: "본문 내용은 양쪽 정렬로 작성됩니다.", size: 24, font: "맑은 고딕" })]
      }),
      // 번호 매기기 예시
      new Paragraph({
        alignment: AlignmentType.JUSTIFIED,
        children: [new TextRun({ text: "1. 첫 번째 항목 (1단계 번호)", size: 24, font: "맑은 고딕" })]
      }),
      new Paragraph({
        alignment: AlignmentType.JUSTIFIED,
        children: [new TextRun({ text: "가. 세부 항목 (2단계 번호)", size: 24, font: "맑은 고딕" })]
      }),
      new Paragraph({
        alignment: AlignmentType.JUSTIFIED,
        children: [new TextRun({ text: "(1) 상세 내용 (3단계 번호)", size: 24, font: "맑은 고딕" })]
      }),
      new Paragraph({
        alignment: AlignmentType.JUSTIFIED,
        children: [new TextRun({ text: "(가) 최하위 항목 (4단계 번호)", size: 24, font: "맑은 고딕" })]
      })
    ]
  }]
});

Packer.toBuffer(doc).then(buffer => {
  fs.writeFileSync("법률문서.docx", buffer);
  console.log("법률문서가 생성되었습니다.");
});
```

### 한국 법률문서 번호 체계 (Korean Legal Numbering Hierarchy)
법률문서의 번호 매김은 다음 계층 구조를 따름:
- **1단계**: 1. 2. 3. (아라비아 숫자 + 마침표)
- **2단계**: 가. 나. 다. (한글 가나다 + 마침표)
- **3단계**: (1) (2) (3) (괄호 + 아라비아 숫자)
- **4단계**: (가) (나) (다) (괄호 + 한글 가나다)

### 법률문서 서식 요약
| 항목 | 설정값 |
|------|--------|
| 본문 정렬 | `AlignmentType.JUSTIFIED` (양쪽정렬) |
| 제목 정렬 | `AlignmentType.CENTER` (가운데정렬) |
| 제목 스타일 | `bold: true` (볼드체) |
| 들여쓰기 | 사용 안함 (indent 속성 제거) |
| 기본 폰트 | 맑은 고딕 (Malgun Gothic) |
| 기본 크기 | 12pt (size: 24) |
| 제목 크기 | 18pt (size: 36) |

**Professional Font Combinations:**
- **Arial (Headers) + Arial (Body)** - Most universally supported, clean and professional
- **Times New Roman (Headers) + Arial (Body)** - Classic serif headers with modern sans-serif body
- **Georgia (Headers) + Verdana (Body)** - Optimized for screen reading, elegant contrast

**Key Styling Principles:**
- **Override built-in styles**: Use exact IDs like "Heading1", "Heading2", "Heading3" to override Word's built-in heading styles
- **HeadingLevel constants**: `HeadingLevel.HEADING_1` uses "Heading1" style, `HeadingLevel.HEADING_2` uses "Heading2" style, etc.
- **Include outlineLevel**: Set `outlineLevel: 0` for H1, `outlineLevel: 1` for H2, etc. to ensure TOC works correctly
- **Use custom styles** instead of inline formatting for consistency
- **Set a default font** using `styles.default.document.run.font` - Arial is universally supported
- **Establish visual hierarchy** with different font sizes (titles > headers > body)
- **Add proper spacing** with `before` and `after` paragraph spacing
- **Use colors sparingly**: Default to black (000000) and shades of gray for titles and headings (heading 1, heading 2, etc.)
- **Set consistent margins** (1440 = 1 inch is standard)


## Lists (ALWAYS USE PROPER LISTS - NEVER USE UNICODE BULLETS)
```javascript
// Bullets - ALWAYS use the numbering config, NOT unicode symbols
// CRITICAL: Use LevelFormat.BULLET constant, NOT the string "bullet"
const doc = new Document({
  numbering: {
    config: [
      { reference: "bullet-list",
        levels: [{ level: 0, format: LevelFormat.BULLET, text: "•", alignment: AlignmentType.LEFT,
          style: { paragraph: { indent: { left: 720, hanging: 360 } } } }] },
      { reference: "first-numbered-list",
        levels: [{ level: 0, format: LevelFormat.DECIMAL, text: "%1.", alignment: AlignmentType.LEFT,
          style: { paragraph: { indent: { left: 720, hanging: 360 } } } }] },
      { reference: "second-numbered-list", // Different reference = restarts at 1
        levels: [{ level: 0, format: LevelFormat.DECIMAL, text: "%1.", alignment: AlignmentType.LEFT,
          style: { paragraph: { indent: { left: 720, hanging: 360 } } } }] }
    ]
  },
  sections: [{
    children: [
      // Bullet list items
      new Paragraph({ numbering: { reference: "bullet-list", level: 0 },
        children: [new TextRun("First bullet point")] }),
      new Paragraph({ numbering: { reference: "bullet-list", level: 0 },
        children: [new TextRun("Second bullet point")] }),
      // Numbered list items
      new Paragraph({ numbering: { reference: "first-numbered-list", level: 0 },
        children: [new TextRun("First numbered item")] }),
      new Paragraph({ numbering: { reference: "first-numbered-list", level: 0 },
        children: [new TextRun("Second numbered item")] }),
      // ⚠️ CRITICAL: Different reference = INDEPENDENT list that restarts at 1
      // Same reference = CONTINUES previous numbering
      new Paragraph({ numbering: { reference: "second-numbered-list", level: 0 },
        children: [new TextRun("Starts at 1 again (because different reference)")] })
    ]
  }]
});

// ⚠️ CRITICAL NUMBERING RULE: Each reference creates an INDEPENDENT numbered list
// - Same reference = continues numbering (1, 2, 3... then 4, 5, 6...)
// - Different reference = restarts at 1 (1, 2, 3... then 1, 2, 3...)
// Use unique reference names for each separate numbered section!

// ⚠️ CRITICAL: NEVER use unicode bullets - they create fake lists that don't work properly
// new TextRun("• Item")           // WRONG
// new SymbolRun({ char: "2022" }) // WRONG
// ✅ ALWAYS use numbering config with LevelFormat.BULLET for real Word lists
```

## Tables
```javascript
// Complete table with margins, borders, headers, and bullet points
const tableBorder = { style: BorderStyle.SINGLE, size: 1, color: "CCCCCC" };
const cellBorders = { top: tableBorder, bottom: tableBorder, left: tableBorder, right: tableBorder };

new Table({
  columnWidths: [4680, 4680], // ⚠️ CRITICAL: Set column widths at table level - values in DXA (twentieths of a point)
  margins: { top: 100, bottom: 100, left: 180, right: 180 }, // Set once for all cells
  rows: [
    new TableRow({
      tableHeader: true,
      children: [
        new TableCell({
          borders: cellBorders,
          width: { size: 4680, type: WidthType.DXA }, // ALSO set width on each cell
          // ⚠️ CRITICAL: Always use ShadingType.CLEAR to prevent black backgrounds in Word.
          shading: { fill: "D5E8F0", type: ShadingType.CLEAR }, 
          verticalAlign: VerticalAlign.CENTER,
          children: [new Paragraph({ 
            alignment: AlignmentType.CENTER,
            children: [new TextRun({ text: "Header", bold: true, size: 22 })]
          })]
        }),
        new TableCell({
          borders: cellBorders,
          width: { size: 4680, type: WidthType.DXA }, // ALSO set width on each cell
          shading: { fill: "D5E8F0", type: ShadingType.CLEAR },
          children: [new Paragraph({ 
            alignment: AlignmentType.CENTER,
            children: [new TextRun({ text: "Bullet Points", bold: true, size: 22 })]
          })]
        })
      ]
    }),
    new TableRow({
      children: [
        new TableCell({
          borders: cellBorders,
          width: { size: 4680, type: WidthType.DXA }, // ALSO set width on each cell
          children: [new Paragraph({ children: [new TextRun("Regular data")] })]
        }),
        new TableCell({
          borders: cellBorders,
          width: { size: 4680, type: WidthType.DXA }, // ALSO set width on each cell
          children: [
            new Paragraph({ 
              numbering: { reference: "bullet-list", level: 0 },
              children: [new TextRun("First bullet point")] 
            }),
            new Paragraph({ 
              numbering: { reference: "bullet-list", level: 0 },
              children: [new TextRun("Second bullet point")] 
            })
          ]
        })
      ]
    })
  ]
})
```

**IMPORTANT: Table Width & Borders**
- Use BOTH `columnWidths: [width1, width2, ...]` array AND `width: { size: X, type: WidthType.DXA }` on each cell
- Values in DXA (twentieths of a point): 1440 = 1 inch, Letter usable width = 9360 DXA (with 1" margins)
- Apply borders to individual `TableCell` elements, NOT the `Table` itself

**Precomputed Column Widths (Letter size with 1" margins = 9360 DXA total):**
- **2 columns:** `columnWidths: [4680, 4680]` (equal width)
- **3 columns:** `columnWidths: [3120, 3120, 3120]` (equal width)

## Links & Navigation
```javascript
// TOC (requires headings) - CRITICAL: Use HeadingLevel only, NOT custom styles
// ❌ WRONG: new Paragraph({ heading: HeadingLevel.HEADING_1, style: "customHeader", children: [new TextRun("Title")] })
// ✅ CORRECT: new Paragraph({ heading: HeadingLevel.HEADING_1, children: [new TextRun("Title")] })
new TableOfContents("Table of Contents", { hyperlink: true, headingStyleRange: "1-3" }),

// External link
new Paragraph({
  children: [new ExternalHyperlink({
    children: [new TextRun({ text: "Google", style: "Hyperlink" })],
    link: "https://www.google.com"
  })]
}),

// Internal link & bookmark
new Paragraph({
  children: [new InternalHyperlink({
    children: [new TextRun({ text: "Go to Section", style: "Hyperlink" })],
    anchor: "section1"
  })]
}),
new Paragraph({
  children: [new TextRun("Section Content")],
  bookmark: { id: "section1", name: "section1" }
}),
```

## Images & Media
```javascript
// Basic image with sizing & positioning
// CRITICAL: Always specify 'type' parameter - it's REQUIRED for ImageRun
new Paragraph({
  alignment: AlignmentType.CENTER,
  children: [new ImageRun({
    type: "png", // NEW REQUIREMENT: Must specify image type (png, jpg, jpeg, gif, bmp, svg)
    data: fs.readFileSync("image.png"),
    transformation: { width: 200, height: 150, rotation: 0 }, // rotation in degrees
    altText: { title: "Logo", description: "Company logo", name: "Name" } // IMPORTANT: All three fields are required
  })]
})
```

## Page Breaks
```javascript
// Manual page break
new Paragraph({ children: [new PageBreak()] }),

// Page break before paragraph
new Paragraph({
  pageBreakBefore: true,
  children: [new TextRun("This starts on a new page")]
})

// ⚠️ CRITICAL: NEVER use PageBreak standalone - it will create invalid XML that Word cannot open
// ❌ WRONG: new PageBreak() 
// ✅ CORRECT: new Paragraph({ children: [new PageBreak()] })
```

## Headers/Footers & Page Setup
```javascript
const doc = new Document({
  sections: [{
    properties: {
      page: {
        margin: { top: 1440, right: 1440, bottom: 1440, left: 1440 }, // 1440 = 1 inch
        size: { orientation: PageOrientation.LANDSCAPE },
        pageNumbers: { start: 1, formatType: "decimal" } // "upperRoman", "lowerRoman", "upperLetter", "lowerLetter"
      }
    },
    headers: {
      default: new Header({ children: [new Paragraph({ 
        alignment: AlignmentType.RIGHT,
        children: [new TextRun("Header Text")]
      })] })
    },
    footers: {
      default: new Footer({ children: [new Paragraph({ 
        alignment: AlignmentType.CENTER,
        children: [new TextRun("Page "), new TextRun({ children: [PageNumber.CURRENT] }), new TextRun(" of "), new TextRun({ children: [PageNumber.TOTAL_PAGES] })]
      })] })
    },
    children: [/* content */]
  }]
});
```

## Tabs
```javascript
new Paragraph({
  tabStops: [
    { type: TabStopType.LEFT, position: TabStopPosition.MAX / 4 },
    { type: TabStopType.CENTER, position: TabStopPosition.MAX / 2 },
    { type: TabStopType.RIGHT, position: TabStopPosition.MAX * 3 / 4 }
  ],
  children: [new TextRun("Left\tCenter\tRight")]
})
```

## Constants & Quick Reference
- **Underlines:** `SINGLE`, `DOUBLE`, `WAVY`, `DASH`
- **Borders:** `SINGLE`, `DOUBLE`, `DASHED`, `DOTTED`  
- **Numbering:** `DECIMAL` (1,2,3), `UPPER_ROMAN` (I,II,III), `LOWER_LETTER` (a,b,c)
- **Tabs:** `LEFT`, `CENTER`, `RIGHT`, `DECIMAL`
- **Symbols:** `"2022"` (•), `"00A9"` (©), `"00AE"` (®), `"2122"` (™), `"00B0"` (°), `"F070"` (✓), `"F0FC"` (✗)

## Critical Issues & Common Mistakes
- **CRITICAL: PageBreak must ALWAYS be inside a Paragraph** - standalone PageBreak creates invalid XML that Word cannot open
- **ALWAYS use ShadingType.CLEAR for table cell shading** - Never use ShadingType.SOLID (causes black background).
- Measurements in DXA (1440 = 1 inch) | Each table cell needs ≥1 Paragraph | TOC requires HeadingLevel styles only
- **ALWAYS use custom styles** with Arial font for professional appearance and proper visual hierarchy
- **ALWAYS set a default font** using `styles.default.document.run.font` - Arial recommended
- **ALWAYS use columnWidths array for tables** + individual cell widths for compatibility
- **NEVER use unicode symbols for bullets** - always use proper numbering configuration with `LevelFormat.BULLET` constant (NOT the string "bullet")
- **NEVER use \n for line breaks anywhere** - always use separate Paragraph elements for each line
- **ALWAYS use TextRun objects within Paragraph children** - never use text property directly on Paragraph
- **CRITICAL for images**: ImageRun REQUIRES `type` parameter - always specify "png", "jpg", "jpeg", "gif", "bmp", or "svg"
- **CRITICAL for bullets**: Must use `LevelFormat.BULLET` constant, not string "bullet", and include `text: "•"` for the bullet character
- **CRITICAL for numbering**: Each numbering reference creates an INDEPENDENT list. Same reference = continues numbering (1,2,3 then 4,5,6). Different reference = restarts at 1 (1,2,3 then 1,2,3). Use unique reference names for each separate numbered section!
- **CRITICAL for TOC**: When using TableOfContents, headings must use HeadingLevel ONLY - do NOT add custom styles to heading paragraphs or TOC will break
- **Tables**: Set `columnWidths` array + individual cell widths, apply borders to cells not table
- **Set table margins at TABLE level** for consistent cell padding (avoids repetition per cell)

### 한국 법률문서 필수 규칙 (Korean Legal Document Rules)
- **본문 양쪽정렬 필수**: `AlignmentType.JUSTIFIED` - 모든 본문 텍스트에 적용
- **제목 가운데정렬 + 볼드 필수**: `AlignmentType.CENTER` + `bold: true`
- **들여쓰기/내어쓰기 금지**: `indent` 속성 사용하지 말 것
- **번호 체계**: 1. → 가. → (1) → (가) 계층구조 준수
- **폰트**: 맑은 고딕 (Malgun Gothic) 사용 권장