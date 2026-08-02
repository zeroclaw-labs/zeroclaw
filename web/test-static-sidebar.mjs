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
  
  console.log('🌐 导航到测试页面...');
  await page.goto('http://localhost:8888/test-sidebar.html', { timeout: 10000 });
  console.log('✅ 页面加载成功');
  
  // 等待测量完成
  console.log('⏳ 等待测量完成...');
  await page.waitForTimeout(2000);
  
  // 获取测量数据
  const measurements = await page.evaluate(() => window.sidebarMeasurements);
  
  if (!measurements) {
    console.log('❌ 无法获取测量数据');
    await browser.close();
    process.exit(1);
  }
  
  console.log('\n📊 测量结果:');
  console.log(`   scrollWidth: ${measurements.scrollWidth}px`);
  console.log(`   clientWidth: ${measurements.clientWidth}px`);
  console.log(`   scrollHeight: ${measurements.scrollHeight}px`);
  console.log(`   clientHeight: ${measurements.clientHeight}px`);
  console.log(`   overflowX: ${measurements.overflowX}`);
  console.log(`   overflowY: ${measurements.overflowY}`);
  
  console.log(`\n🔍 水平溢出检查: ${measurements.hasHorizontalOverflow ? '❌ 存在溢出' : '✅ 无溢出'}`);
  
  // 截图
  const screenshotPath = path.join(__dirname, 'docs/evidence/8877/r4/compact-viewport-800x500-static.png');
  await page.screenshot({ path: screenshotPath, fullPage: false });
  console.log(`\n📸 截图已保存到: ${screenshotPath}`);
  
  // 测试 tooltip
  console.log('\n🔍 测试 tooltip 渲染...');
  const firstNavItem = await page.$('.nav-item');
  if (firstNavItem) {
    await firstNavItem.hover();
    await page.waitForTimeout(500);
    
    const tooltip = await page.$('.tooltip.visible');
    if (tooltip) {
      const tooltipRect = await tooltip.boundingBox();
      console.log(`✅ Tooltip 渲染成功`);
      console.log(`   位置: left=${tooltipRect.x.toFixed(1)}px, top=${tooltipRect.y.toFixed(1)}px`);
      
      const tooltipScreenshotPath = path.join(__dirname, 'docs/evidence/8877/r4/compact-viewport-tooltip-static.png');
      await tooltip.screenshot({ path: tooltipScreenshotPath });
      console.log(`📸 Tooltip 截图已保存到: ${tooltipScreenshotPath}`);
    } else {
      console.log('❌ Tooltip 未渲染');
    }
  }
  
  // 保存测量数据
  const measurementsPath = path.join(__dirname, 'docs/evidence/8877/r4/measurements-800x500-static.json');
  fs.writeFileSync(measurementsPath, JSON.stringify({
    viewport: { width: 800, height: 500 },
    timestamp: new Date().toISOString(),
    branch: 'fix/8791-sidebar-overflow-x',
    testPage: 'test-sidebar.html (static HTML simulating Sidebar.tsx)',
    ...measurements
  }, null, 2));
  console.log(`\n📄 测量数据已保存到: ${measurementsPath}`);
  
  await browser.close();
  console.log('\n✅ 测试完成！');
  
  if (measurements.hasHorizontalOverflow) {
    console.log('\n❌ 失败：在 800×500 视口下存在水平溢出');
    process.exit(1);
  } else {
    console.log('\n✅ 成功：在 800×500 视口下无水平溢出');
  }
})();
