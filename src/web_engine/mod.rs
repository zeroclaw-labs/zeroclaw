use anyhow::Result;
#[cfg(feature = "browser-native")]
use std::sync::Arc;
#[cfg(feature = "browser-native")]
use tokio::sync::Mutex;

#[cfg(feature = "browser-native")]
use chromiumoxide::browser::{Browser, BrowserConfig};
#[cfg(feature = "browser-native")]
use chromiumoxide::handler::viewport::Viewport;
#[cfg(feature = "browser-native")]
use chromiumoxide::page::Page;
#[cfg(feature = "browser-native")]
use futures_util::StreamExt;
#[cfg(feature = "browser-native")]
use tracing::{debug, error};

#[cfg(feature = "browser-native")]
pub struct BrowserBackend {
    browser: Browser,
    _handler_handle: tokio::task::JoinHandle<()>,
}

#[cfg(feature = "browser-native")]
impl BrowserBackend {
    pub async fn launch(headless: bool, chrome_path: Option<&str>) -> Result<Self> {
        let mut builder = BrowserConfig::builder()
            .viewport(Viewport {
                width: 1280,
                height: 800,
                ..Default::default()
            });

        if headless {
            builder = builder.with_head_less();
        } else {
            builder = builder.with_head_full();
        }

        if let Some(path) = chrome_path {
            builder = builder.chrome_executable(path);
        }

        let (browser, mut handler) = Browser::launch(builder.build().map_err(|e| anyhow::anyhow!(e))?)
            .await
            .context("Failed to launch chromium")?;

        let _handler_handle = tokio::spawn(async move {
            while let Some(h) = handler.next().await {
                if let Err(e) = h {
                    error!("Browser handler error: {}", e);
                    break;
                }
            }
        });

        Ok(Self {
            browser,
            _handler_handle,
        })
    }

    pub async fn new_page(&self, url: &str) -> Result<Page> {
        let page = self.browser.new_page(url).await.context("Failed to create new page")?;
        Ok(page)
    }

    pub async fn close(self) -> Result<()> {
        self.browser.close().await.context("Failed to close browser")?;
        Ok(())
    }
}

pub struct ChromiumManager {
    #[cfg(feature = "browser-native")]
    backend: Arc<Mutex<Option<BrowserBackend>>>,
}

impl ChromiumManager {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "browser-native")]
            backend: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(feature = "browser-native")]
    pub async fn get_page(&self, headless: bool, chrome_path: Option<&str>) -> Result<Page> {
        let mut lock = self.backend.lock().await;
        if lock.is_none() {
            debug!("Launching new browser instance");
            *lock = Some(BrowserBackend::launch(headless, chrome_path).await?);
        }
        
        let backend = lock.as_ref().unwrap();
        // Return the first page or create a new one
        let pages = backend.browser.pages().await?;
        if let Some(page) = pages.first() {
            Ok(page.clone())
        } else {
            backend.new_page("about:blank").await
        }
    }

    pub async fn close(&self) -> Result<()> {
        #[cfg(feature = "browser-native")]
        {
            let mut lock = self.backend.lock().await;
            if let Some(backend) = lock.take() {
                backend.close().await?;
            }
        }
        Ok(())
    }
}

impl Default for ChromiumManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Core Actions ─────────────────────────────────────────────────────────────

#[cfg(feature = "browser-native")]
pub async fn navigate(page: &Page, url: &str) -> Result<()> {
    page.goto(url).await?;
    page.wait_for_navigation().await?;
    Ok(())
}

#[cfg(feature = "browser-native")]
pub async fn click(page: &Page, selector: &str) -> Result<()> {
    page.find_element(selector)
        .await
        .context(format!("Failed to find element for clicking: {selector}"))?
        .click()
        .await
        .context(format!("Failed to click element: {selector}"))?;
    Ok(())
}

#[cfg(feature = "browser-native")]
pub async fn type_text(page: &Page, selector: &str, text: &str) -> Result<()> {
    let element = page.find_element(selector)
        .await
        .context(format!("Failed to find element for typing: {selector}"))?;
    
    element.click().await?; // Focus
    element.type_str(text).await.context(format!("Failed to type text into: {selector}"))?;
    Ok(())
}

#[cfg(feature = "browser-native")]
pub async fn get_content(page: &Page) -> Result<String> {
    page.content().await.context("Failed to get page content")
}

pub async fn open_in_brave(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        for app in ["Brave Browser", "Brave"] {
            let status = tokio::process::Command::new("open")
                .arg("-a")
                .arg(app)
                .arg(url)
                .status()
                .await;

            if let Ok(s) = status {
                if s.success() {
                    return Ok(());
                }
            }
        }
        anyhow::bail!(
            "Brave Browser was not found (tried macOS app names 'Brave Browser' and 'Brave')"
        );
    }

    #[cfg(target_os = "linux")]
    {
        let mut last_error = String::new();
        for cmd in ["brave-browser", "brave"] {
            match tokio::process::Command::new(cmd).arg(url).status().await {
                Ok(status) if status.success() => return Ok(()),
                Ok(status) => {
                    last_error = format!("{cmd} exited with status {status}");
                }
                Err(e) => {
                    last_error = format!("{cmd} not runnable: {e}");
                }
            }
        }
        anyhow::bail!("{last_error}");
    }

    #[cfg(target_os = "windows")]
    {
        let status = tokio::process::Command::new("cmd")
            .args(["/C", "start", "", "brave", url])
            .status()
            .await?;

        if status.success() {
            return Ok(());
        }

        anyhow::bail!("cmd start brave exited with status {status}");
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = url;
        anyhow::bail!("browser launch is not supported on this OS");
    }
}

#[cfg(feature = "browser-native")]
pub async fn screenshot(page: &Page) -> Result<Vec<u8>> {
    page.screenshot(chromiumoxide::page::ScreenshotParams::builder().build())
        .await
        .context("Failed to take screenshot")
}

#[cfg(feature = "browser-native")]
pub async fn snapshot(
    page: &Page,
    interactive_only: bool,
    compact: bool,
    depth: Option<u32>,
) -> Result<serde_json::Value> {
    let depth_literal = depth
        .map(|level| level.to_string())
        .unwrap_or_else(|| "null".to_string());

    let script = format!(
        r#"(() => {{
  const interactiveOnly = {interactive_only};
  const compact = {compact};
  const maxDepth = {depth_literal};
  const nodes = [];
  const root = document.body || document.documentElement;
  let counter = 0;

  const isVisible = (el) => {{
    const style = window.getComputedStyle(el);
    if (style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity || 1) === 0) {{
      return false;
    }}
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  }};

  const isInteractive = (el) => {{
    if (el.matches('a,button,input,select,textarea,summary,[role],*[tabindex]')) return true;
    return typeof el.onclick === 'function';
  }};

  const describe = (el, depth) => {{
    const interactive = isInteractive(el);
    const text = (el.innerText || el.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 140);
    if (interactiveOnly && !interactive) return;
    if (compact && !interactive && !text) return;

    const ref = '@e' + (++counter);
    el.setAttribute('data-zc-ref', ref);
    nodes.push({{
      ref,
      depth,
      tag: el.tagName.toLowerCase(),
      id: el.id || null,
      role: el.getAttribute('role'),
      text,
      interactive,
    }});
  }};

  const walk = (el, depth) => {{
    if (!(el instanceof Element)) return;
    if (maxDepth !== null && depth > maxDepth) return;
    if (isVisible(el)) {{
      describe(el, depth);
    }}
    for (const child of el.children) {{
      walk(child, depth + 1);
      if (nodes.length >= 400) return;
    }}
  }};

  if (root) walk(root, 0);

  return {{
    title: document.title,
    url: window.location.href,
    count: nodes.length,
    nodes,
  }};
}})();"#
    );

    let result = page.evaluate(script).await.context("Failed to execute snapshot script")?;
    Ok(result.into_value())
}
