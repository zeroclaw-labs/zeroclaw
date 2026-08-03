import { chromium } from 'playwright';
import path from 'path';
import { fileURLToPath } from 'url';
import fs from 'fs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

(async () => {
  console.log('🚀 启动浏览器...');
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    viewport: { width: 800, height: 500 }
  });
  const page = await context.newPage();
  
  console.log('📱 设置视口为 800×500...');
  await page.setViewportSize({ width: 800, height: 500 });
  
  console.log('🌐 导航到本地 Vite 服务器...');
  await page.goto('http://localhost:4173', { timeout: 30000, waitUntil: 'networkidle' });
  console.log('✅ 页面加载成功');
  
  // 等待 React 渲染
  console.log('⏳ 等待 React 渲染...');
  await page.waitForTimeout(3000);
  
  // 尝试不同的选择器
  console.log('🔍 查找 Sidebar...');
  const sidebar = await page.$('aside.md\\:hidden');
  if (!sidebar) {
    console.log('❌ 找不到 Sidebar，尝试其他选择器...');
    const allAsides = await page.$$('aside');
    console.log(`找到 ${allAsides.length} 个 aside 元素`);
    for (let i = 0; i < allAsides.length; i++) {
      const ariaLabel = await allAsides[i].getAttribute('aria-label');
      console.log(`  aside[${i}]: aria-label="${ariaLabel}"`);
    }
    await browser.close();
    process.exit(1);
  }
  
  const nav = await sidebar.$('nav');
  if (!nav) {
    console.log('❌ 找不到 nav 元素');
    await browser.close();
    process.exit(1);
  }
  
  const measurements = await nav.evaluate(el => ({
    scrollWidth: el.scrollWidth,
    clientWidth: el.clientWidth,
    scrollHeight: el.scrollHeight,
    clientHeight: el.clientHeight,
    overflowX: window.getComputedStyle(el).overflowX,
    overflowY: window.getComputedStyle(el).overflowY
  }));
  
  console.log('\n📊 测量结果:');
  console.log(`   scrollWidth: ${measurements.scrollWidth}px`);
  console.log(`   clientWidth: ${measurements.clientWidth}px`);
  console.log(`   scrollHeight: ${measurements.scrollHeight}px`);
  console.log(`   clientHeight: ${measurements.clientHeight}px`);
  console.log(`   overflowX: ${measurements.overflowX}`);
  console.log(`   overflowY: ${measurements.overflowY}`);
  
  const hasHorizontalOverflow = measurements.scrollWidth > measurements.clientWidth;
  console.log(`\n🔍 水平溢出检查: ${hasHorizontalOverflow ? '❌ 存在溢出' : '✅ 无溢出'}`);
  
  const screenshotPath = path.join(__dirname, 'docs/evidence/8877/r4/compact-viewport-800x500.png');
  await page.screenshot({ path: screenshotPath, fullPage: false });
  console.log(`\n📸 截图已保存到: ${screenshotPath}`);
  
  console.log('\n🔍 测试 tooltip 渲染...');
  const firstNavLink = await sidebar.$('nav a');
  if (firstNavLink) {
    await firstNavLink.hover();
    await page.waitForTimeout(500);
    
    const tooltip = await page.$('[role="tooltip"]');
    if (tooltip) {
      const tooltipRect = await tooltip.boundingBox();
      console.log(`✅ Tooltip 渲染成功`);
      console.log(`   位置: left=${tooltipRect.x.toFixed(1)}px, top=${tooltipRect.y.toFixed(1)}px`);
      
      const tooltipScreenshotPath = path.join(__dirname, 'docs/evidence/8877/r4/compact-viewport-tooltip.png');
      await tooltip.screenshot({ path: tooltipScreenshotPath });
      console.log(`📸 Tooltip 截图已保存到: ${tooltipScreenshotPath}`);
    } else {
      console.log('❌ Tooltip 未渲染');
    }
  }
  
  const measurementsPath = path.join(__dirname, 'docs/evidence/8877/r4/measurements-800x500.json');
  fs.writeFileSync(measurementsPath, JSON.stringify({
    viewport: { width: 800, height: 500 },
    timestamp: new Date().toISOString(),
    branch: 'fix/8791-sidebar-overflow-x',
    ...measurements,
    hasHorizontalOverflow,
    pass: !hasHorizontalOverflow
  }, null, 2));
  console.log(`\n📄 测量数据已保存到: ${measurementsPath}`);
  
  await browser.close();
  console.log('\n✅ 测试完成！');
  
  if (hasHorizontalOverflow) {
    console.log('\n❌ 失败：在 800×500 视口下存在水平溢出');
    process.exit(1);
  } else {
    console.log('\n✅ 成功：在 800×500 视口下无水平溢出');
  }
})();
