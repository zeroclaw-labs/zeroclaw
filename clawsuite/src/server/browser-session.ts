/**
 * Server-side browser session powered by Playwright.
 * Manages a single Chromium instance with one active page.
 * Provides screenshot streaming, navigation, click, and type operations.
 */

import type { Browser, Page, BrowserContext } from 'playwright'

let browserInstance: Browser | null = null
let contextInstance: BrowserContext | null = null
let pageInstance: Page | null = null
let lastScreenshot: string | null = null
let lastUrl = ''
let lastTitle = ''
let isLaunching = false
let screencastRunning = false
let cdpSession: any = null

const VIEWPORT = { width: 1280, height: 800 }
const SCREENSHOT_TIMEOUT = 10_000
const NAV_TIMEOUT = 30_000

export type BrowserState = {
  running: boolean
  url: string
  title: string
  screenshot: string | null // base64 data URL
}

async function getPlaywright() {
  // Dynamic import so it doesn't break if not installed
  const pw = await import('playwright')
  return pw
}

export async function launchBrowser(): Promise<BrowserState> {
  if (browserInstance && pageInstance) {
    return getState()
  }

  if (isLaunching) {
    // Wait for ongoing launch
    await new Promise((r) => setTimeout(r, 2000))
    return getState()
  }

  isLaunching = true
  try {
    const pw = await getPlaywright()
    // Headless browser — rendered inside ClawSuite via CDP screencast
    browserInstance = await pw.chromium.launch({
      headless: true,
      args: [
        '--no-sandbox',
        '--disable-setuid-sandbox',
        '--disable-dev-shm-usage',
      ],
    })

    contextInstance = await browserInstance.newContext({
      viewport: VIEWPORT,
      userAgent:
        'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36',
    })

    pageInstance = await contextInstance.newPage()
    await pageInstance.goto('about:blank')

    // Start CDP screencast — pushes frames on change instead of polling
    await startScreencast()
    await captureScreenshot()

    return getState()
  } finally {
    isLaunching = false
  }
}

async function startScreencast(): Promise<void> {
  if (!pageInstance || screencastRunning) return
  try {
    cdpSession = await pageInstance.context().newCDPSession(pageInstance)
    cdpSession.on(
      'Page.screencastFrame',
      (params: { data: string; sessionId: number }) => {
        lastScreenshot = `data:image/jpeg;base64,${params.data}`
        // Acknowledge frame so Chrome keeps sending
        cdpSession
          .send('Page.screencastFrameAck', { sessionId: params.sessionId })
          .catch(() => {})
      },
    )
    await cdpSession.send('Page.startScreencast', {
      format: 'jpeg',
      quality: 80,
      maxWidth: VIEWPORT.width,
      maxHeight: VIEWPORT.height,
      everyNthFrame: 1,
    })
    screencastRunning = true
  } catch {
    // Fallback to manual screenshots if CDP screencast isn't available
    screencastRunning = false
  }
}

async function stopScreencast(): Promise<void> {
  if (cdpSession && screencastRunning) {
    try {
      await cdpSession.send('Page.stopScreencast')
    } catch {}
    screencastRunning = false
  }
  cdpSession = null
}

// CDP input dispatch — sends real mouse/keyboard events, way more reliable than Playwright click
export async function cdpMouseClick(
  x: number,
  y: number,
): Promise<BrowserState> {
  if (!cdpSession || !pageInstance) return clickAt(x, y)
  try {
    await cdpSession.send('Input.dispatchMouseEvent', {
      type: 'mousePressed',
      x,
      y,
      button: 'left',
      clickCount: 1,
    })
    await cdpSession.send('Input.dispatchMouseEvent', {
      type: 'mouseReleased',
      x,
      y,
      button: 'left',
      clickCount: 1,
    })
    // Give page time to react
    await pageInstance.waitForTimeout(200)
    lastUrl = pageInstance.url()
    lastTitle = await pageInstance.title().catch(() => '')
  } catch {
    await pageInstance.mouse.click(x, y)
  }
  return getState()
}

export async function cdpMouseMove(x: number, y: number): Promise<void> {
  if (!cdpSession) return
  try {
    await cdpSession.send('Input.dispatchMouseEvent', {
      type: 'mouseMoved',
      x,
      y,
    })
  } catch {}
}

export async function closeBrowser(): Promise<void> {
  await stopScreencast()
  if (contextInstance) {
    await contextInstance.close().catch(() => {})
    contextInstance = null
  }
  if (browserInstance) {
    await browserInstance.close().catch(() => {})
    browserInstance = null
  }
  pageInstance = null
  lastScreenshot = null
  lastUrl = ''
  lastTitle = ''
}

export async function navigate(url: string): Promise<BrowserState> {
  if (!pageInstance) await launchBrowser()
  if (!pageInstance) throw new Error('Browser not available')

  // Auto-add https:// if no protocol
  let normalizedUrl = url.trim()
  if (normalizedUrl && !normalizedUrl.match(/^https?:\/\//)) {
    normalizedUrl = `https://${normalizedUrl}`
  }

  await pageInstance.goto(normalizedUrl, {
    waitUntil: 'domcontentloaded',
    timeout: NAV_TIMEOUT,
  })

  // Wait a bit for rendering
  await pageInstance.waitForTimeout(500)
  await captureScreenshot()
  return getState()
}

export async function clickAt(x: number, y: number): Promise<BrowserState> {
  if (!pageInstance) throw new Error('Browser not running')
  await pageInstance.mouse.click(x, y)
  await pageInstance.waitForTimeout(300)
  await captureScreenshot()
  return getState()
}

export async function typeText(
  text: string,
  submit = false,
): Promise<BrowserState> {
  if (!pageInstance) throw new Error('Browser not running')
  await pageInstance.keyboard.type(text, { delay: 30 })
  if (submit) {
    await pageInstance.keyboard.press('Enter')
    await pageInstance.waitForTimeout(500)
  }
  await captureScreenshot()
  return getState()
}

export async function pressKey(key: string): Promise<BrowserState> {
  if (!pageInstance) throw new Error('Browser not running')
  await pageInstance.keyboard.press(key)
  await pageInstance.waitForTimeout(300)
  await captureScreenshot()
  return getState()
}

export async function goBack(): Promise<BrowserState> {
  if (!pageInstance) throw new Error('Browser not running')
  await pageInstance.goBack({ timeout: NAV_TIMEOUT }).catch(() => {})
  await pageInstance.waitForTimeout(500)
  await captureScreenshot()
  return getState()
}

export async function goForward(): Promise<BrowserState> {
  if (!pageInstance) throw new Error('Browser not running')
  await pageInstance.goForward({ timeout: NAV_TIMEOUT }).catch(() => {})
  await pageInstance.waitForTimeout(500)
  await captureScreenshot()
  return getState()
}

export async function refresh(): Promise<BrowserState> {
  if (!pageInstance) throw new Error('Browser not running')
  await pageInstance.reload({ timeout: NAV_TIMEOUT }).catch(() => {})
  await pageInstance.waitForTimeout(500)
  await captureScreenshot()
  return getState()
}

export async function scrollPage(
  direction: 'up' | 'down',
  amount = 400,
): Promise<BrowserState> {
  if (!pageInstance) throw new Error('Browser not running')
  const delta = direction === 'down' ? amount : -amount
  await pageInstance.mouse.wheel(0, delta)
  await pageInstance.waitForTimeout(300)
  await captureScreenshot()
  return getState()
}

export async function getScreenshot(): Promise<BrowserState> {
  if (!pageInstance) {
    return { running: false, url: '', title: '', screenshot: null }
  }
  await captureScreenshot()
  return getState()
}

async function captureScreenshot(): Promise<void> {
  if (!pageInstance) return
  try {
    const buffer = await pageInstance.screenshot({
      type: 'png',
      timeout: SCREENSHOT_TIMEOUT,
    })
    lastScreenshot = `data:image/png;base64,${buffer.toString('base64')}`
    lastUrl = pageInstance.url()
    lastTitle = await pageInstance.title().catch(() => '')
  } catch {
    // Page might be navigating
  }
}

export async function getPageContent(): Promise<{
  url: string
  title: string
  text: string
}> {
  if (!pageInstance) return { url: '', title: '', text: '' }
  const url = pageInstance.url()
  const title = await pageInstance.title().catch(() => '')
  const text = await pageInstance
    .evaluate(() => {
      // Extract readable text content
      const body = document.body
      if (!body) return ''
      // Remove scripts and styles
      const clone = body.cloneNode(true) as HTMLElement
      clone
        .querySelectorAll('script, style, noscript, svg')
        .forEach((el) => el.remove())
      return (clone.textContent || '')
        .replace(/\s+/g, ' ')
        .trim()
        .slice(0, 8000)
    })
    .catch(() => '')
  return { url, title, text }
}

function getState(): BrowserState {
  return {
    running: !!pageInstance,
    url: lastUrl,
    title: lastTitle,
    screenshot: lastScreenshot,
  }
}
