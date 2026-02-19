#!/usr/bin/env node
import { chromium } from 'playwright'
import { readFileSync, writeFileSync } from 'fs'
import { join, dirname } from 'path'
import { fileURLToPath } from 'url'

const __filename = fileURLToPath(import.meta.url)
const __dirname = dirname(__filename)

const svgPath = join(__dirname, '../public/logo-icon-simple.svg')
const outputDir = join(__dirname, '../public')

async function generateIcon(size) {
  const browser = await chromium.launch()
  const page = await browser.newPage({
    viewport: { width: size, height: size },
  })

  const svgContent = readFileSync(svgPath, 'utf-8')
  const html = `
    <!DOCTYPE html>
    <html>
      <head>
        <style>
          body { margin: 0; padding: 0; }
          svg { width: 100%; height: 100%; }
        </style>
      </head>
      <body>${svgContent}</body>
    </html>
  `

  await page.setContent(html)
  const screenshot = await page.screenshot({ type: 'png' })
  await browser.close()

  const outputPath = join(outputDir, `clawsuite-icon-${size}.png`)
  writeFileSync(outputPath, screenshot)
  console.log(`✓ Generated ${size}x${size} icon`)
}

async function main() {
  console.log('Generating PWA icons...')
  await generateIcon(192)
  await generateIcon(512)
  console.log('✓ All icons generated successfully!')
}

main().catch(console.error)
