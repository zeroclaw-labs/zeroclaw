import { chromium } from 'playwright';

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage();

console.log('🌐 导航到页面...');
await page.goto('http://localhost:4173', { timeout: 30000, waitUntil: 'networkidle' });
console.log('✅ 页面加载成功');

// 等待 React 渲染
await page.waitForTimeout(5000);

// 获取页面标题
const title = await page.title();
console.log(`📄 页面标题：${title}`);

// 查找所有 aside 元素
const asideCount = await page.$$eval('aside', els => els.length);
console.log(`🔍 找到 ${asideCount} 个 aside 元素`);

// 获取所有 aside 的详细信息
const asideDetails = await page.$$eval('aside', asides => 
  asides.map((a, i) => ({
    index: i,
    class: a.className,
    ariaLabel: a.getAttribute('aria-label'),
    id: a.id,
    style: a.getAttribute('style')?.substring(0, 100)
  }))
);
console.log('\n📋 Aside 详情:');
asideDetails.forEach(aside => {
  console.log(`  [${aside.index}] class="${aside.class}", aria-label="${aside.ariaLabel}"`);
});

await browser.close();
