import { expect, test, type Page } from '@playwright/test';
import { getLanguageOption, getLocaleDirection, LANGUAGE_OPTIONS, LANGUAGE_SWITCH_ORDER, tLocale, type Locale } from '../src/lib/i18n';

const routes: Array<{
  path: string;
  titleKey: string;
  assertion?: (page: Page, locale: Locale) => Promise<void>;
}> = [
  {
    path: '/_app/',
    titleKey: 'nav.dashboard',
    assertion: (page, locale) => expect(page.getByText(tLocale('dashboard.hero_title', locale), { exact: false })).toBeVisible(),
  },
  {
    path: '/_app/agent',
    titleKey: 'nav.agent',
    assertion: (page, locale) => expect(page.getByPlaceholder(tLocale('agent.placeholder', locale))).toBeVisible(),
  },
  {
    path: '/_app/tools',
    titleKey: 'nav.tools',
    assertion: (page, locale) => expect(page.getByPlaceholder(tLocale('tools.search', locale))).toBeVisible(),
  },
  {
    path: '/_app/cron',
    titleKey: 'nav.cron',
    assertion: (page, locale) => expect(page.getByRole('button', { name: tLocale('cron.add', locale) })).toBeVisible(),
  },
  { path: '/_app/integrations', titleKey: 'nav.integrations' },
  {
    path: '/_app/memory',
    titleKey: 'nav.memory',
    assertion: (page, locale) => expect(page.getByRole('button', { name: tLocale('memory.add_memory', locale) })).toBeVisible(),
  },
  {
    path: '/_app/config',
    titleKey: 'nav.config',
    assertion: (page, locale) => expect(page.getByRole('button', { name: tLocale('config.save', locale) })).toBeVisible(),
  },
  {
    path: '/_app/cost',
    titleKey: 'nav.cost',
    assertion: (page, locale) => expect(page.getByText(tLocale('cost.token_statistics', locale), { exact: false })).toBeVisible(),
  },
  {
    path: '/_app/logs',
    titleKey: 'nav.logs',
    assertion: (page, locale) => expect(page.getByText(tLocale('logs.title', locale), { exact: false })).toBeVisible(),
  },
  {
    path: '/_app/doctor',
    titleKey: 'nav.doctor',
    assertion: (page, locale) => expect(page.getByRole('heading', { name: tLocale('doctor.title', locale) }).first()).toBeVisible(),
  },
];

async function chooseLocale(page: Page, locale: Locale) {
  if (await page.locator('html').getAttribute('lang') === locale) {
    await expect(page.getByTestId('locale-flag')).toHaveText(getLanguageOption(locale).flag);
    return;
  }

  await page.getByTestId('locale-select').click();
  await expect(page.getByTestId('locale-menu')).toBeVisible();
  await page.getByTestId(`locale-option-${locale}`).click();
  await expect(page.getByTestId('locale-menu')).toBeHidden();
}

test.describe('localization', () => {
  test('locale selector lists every configured language', async ({ page }) => {
    await page.goto('/_app/');
    await page.getByTestId('locale-select').click();
    await expect(page.getByTestId('locale-menu')).toBeVisible();

    for (const option of LANGUAGE_OPTIONS) {
      const optionLocator = page.getByTestId(`locale-option-${option.value}`);
      await expect(optionLocator).toContainText(option.flag);
      await expect(optionLocator).toContainText(option.label);
    }
  });

  test('pairing screen supports every locale', async ({ page }) => {
    await page.goto('/_app/');

    for (const locale of LANGUAGE_SWITCH_ORDER) {
      await chooseLocale(page, locale);
      await expect(page.getByTestId('locale-flag')).toHaveText(getLanguageOption(locale).flag);
      await expect(page.locator('html')).toHaveAttribute('lang', locale);
      await expect(page.locator('html')).toHaveAttribute('dir', getLocaleDirection(locale));
      await expect(page.locator('body')).toHaveAttribute('dir', getLocaleDirection(locale));
      await expect(page.getByTestId('pair-button')).toHaveText(tLocale('auth.pair_button', locale));
      await expect(page.getByPlaceholder(tLocale('auth.code_placeholder', locale))).toBeVisible();
      await expect(page.getByText(tLocale('auth.enter_code', locale))).toBeVisible();
    }
  });

  for (const locale of LANGUAGE_SWITCH_ORDER) {
    test(`authenticated dashboard shell localizes cleanly for ${locale}`, async ({ page }) => {
      await page.addInitScript((selectedLocale) => {
        window.localStorage.setItem('zeroclaw_token', 'test-token');
        window.localStorage.setItem('zeroclaw:locale', selectedLocale);
      }, locale);

      for (const route of routes) {
        await page.goto(route.path);
        await chooseLocale(page, locale);
        await expect(page.getByTestId('locale-flag')).toHaveText(getLanguageOption(locale).flag);
        await expect(page.locator('html')).toHaveAttribute('lang', locale);
        await expect(page.locator('html')).toHaveAttribute('dir', getLocaleDirection(locale));
        await expect(page.locator('body')).toHaveAttribute('dir', getLocaleDirection(locale));
        await expect(page.locator('header h1')).toHaveText(tLocale(route.titleKey, locale));
        if (route.assertion) {
          await route.assertion(page, locale);
        }
      }
    });
  }
});
