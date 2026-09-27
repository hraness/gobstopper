import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { isAbsolute, resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { chromium } from 'playwright-core';

const { values } = parseArgs({ options: { production: { type: 'boolean', default: false } }, strict: true });
const repository = resolve(import.meta.dirname, '..');
const artifacts = resolve(repository, '.impeccable/review', `public-${Date.now()}`);
await mkdir(artifacts, { recursive: true });
const routes = ['/', '/docs', '/benchmarks', '/methodology', '/compare/claude-code-compact', '/compare/cliffcompaction', '/blog', '/blog/introducing-gobstopper', '/blog/proofs-for-the-admission-math', '/blog/vault-models-that-fail-on-purpose', '/missing-public-verification'];
const errors = [];
const records = [];
const startedAt = Date.now();
let server;
let browser;
let activePage;
let cleanupPromise;
let origin = 'https://gobstopper.sh';
const pause = (ms) => new Promise(resolve => setTimeout(resolve, ms));
async function until(check, label, timeout = 5000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) { if (await check()) return; await pause(50); }
  throw new Error(`Timed out: ${label}`);
}
async function stopServer() {
  if (!server || server.exitCode !== null || server.signalCode !== null) return;
  const exited = once(server, 'exit');
  server.kill('SIGTERM');
  await Promise.race([exited, pause(5000)]);
  if (server.exitCode === null && server.signalCode === null) { server.kill('SIGKILL'); await exited; }
}
function cleanup() {
  return cleanupPromise ??= (async () => {
    try { await browser?.close(); } finally { await stopServer(); }
  })();
}
const deadline = setTimeout(() => { console.error('Public browser verification exceeded four minutes.'); void cleanup().finally(() => process.exit(1)); }, 240000);
for (const signal of ['SIGINT', 'SIGTERM']) process.once(signal, () => { void cleanup().finally(() => process.exit(1)); });
try {
  if (!values.production) {
    const socket = createServer();
    socket.listen(0, '127.0.0.1'); await once(socket, 'listening');
    const port = socket.address().port;
    await new Promise(resolve => socket.close(resolve));
    origin = `http://127.0.0.1:${port}`;
    server = spawn(process.execPath, [resolve(repository, 'node_modules/next/dist/bin/next'), 'start', '--hostname', '127.0.0.1', '--port', String(port)], {
      cwd: repository, stdio: ['ignore', 'inherit', 'inherit'], env: process.env,
    });
    server.once('error', error => errors.push(`Server: ${error.message}`));
    await until(async () => { assert.equal(server.exitCode, null, 'Owned Next server exited'); return fetch(origin, { signal: AbortSignal.timeout(1000) }).then(r => r.ok, () => false); }, 'Next production server', 30000);
  }
  const executablePath = process.env.GOBSTOPPER_BROWSER_EXECUTABLE;
  if (executablePath) assert.ok(isAbsolute(executablePath), 'Explicit browser executable must be absolute');
  browser = await chromium.launch({ headless: true, ...(executablePath ? { executablePath } : { channel: 'chrome' }) });
  for (const width of [360, 390, 1440]) for (const theme of ['light', 'dark']) {
    const context = await browser.newContext({ viewport: { width, height: width === 360 ? 740 : width === 390 ? 844 : 900 }, colorScheme: theme, reducedMotion: 'reduce', serviceWorkers: 'block' });
    const page = await context.newPage(); activePage = page;
    page.setDefaultTimeout(10000);
    page.on('pageerror', error => errors.push(error.message));
    page.on('console', message => { if (message.type() === 'error' && !message.location().url.includes('/missing-public-verification')) errors.push(message.text()); });
    page.on('response', response => { if (response.status() >= 400 && !response.url().includes('/missing-public-verification')) errors.push(`HTTP ${response.status()} ${new URL(response.url()).pathname}`); });
    for (const path of routes) {
      const label = `${path === '/' ? 'home' : path.slice(1).replaceAll('/', '-')}-${width}-${theme}`;
      const response = await page.goto(origin + path, { waitUntil: 'load' });
      assert.equal(response.status(), path === '/missing-public-verification' ? 404 : 200, label);
      await page.evaluate(async () => { await document.fonts.ready; });
      assert.match(await page.title(), /Gobstopper/i, label);
      assert.equal(await page.locator('h1').count(), 1, label);
      assert.equal(await page.locator('#hraness-site-footer').count(), 1, label);
      assert.equal(await page.locator('iframe').count(), 0, 'Retired embedded preview stays absent');
      await until(() => page.evaluate(() => document.documentElement.dataset.theme).then(value => value === theme), `${label}: resolved theme`);
      await page.screenshot({ path: resolve(artifacts, `${label}.jpg`), type: 'jpeg', quality: 90, fullPage: true, animations: 'disabled' });
      const metrics = await page.evaluate(() => {
        const header = document.querySelector('.hraness-marketing-header');
        const footer = document.querySelector('#hraness-site-footer');
        const inner = footer.querySelector('.hraness-site-footer__inner');
        const main = document.querySelector('main');
        const rect = element => { const r = element.getBoundingClientRect(); return { top: r.top, bottom: r.bottom, left: r.left, right: r.right, height: r.height, width: r.width }; };
        return { overflow: document.documentElement.scrollWidth - innerWidth, header: rect(header), headerPosition: getComputedStyle(header).position, footer: rect(footer), footerInner: rect(inner), footerPosition: getComputedStyle(inner).position, main: rect(main), font: getComputedStyle(document.body).fontFamily, targets: [...header.querySelectorAll('a, button, summary')].map(a => ({ label: a.getAttribute('href') ?? a.getAttribute('aria-label'), navigation: Boolean(a.closest('nav')), ...rect(a) })) };
      });
      assert.ok(metrics.overflow <= 1, `${label}: document overflow`);
      assert.match(metrics.font, /Nebula Sans/, label);
      assert.equal(metrics.headerPosition, 'sticky', label);
      assert.ok(metrics.header.height <= (width < 600 ? 140 : 90), `${label}: header height`);
      assert.ok(['static', 'relative'].includes(metrics.footerPosition), `${label}: footer in flow`);
      assert.ok(metrics.footer.top >= metrics.main.bottom - 1, `${label}: footer follows main`);
      assert.ok(metrics.footer.height >= metrics.footerInner.height - 1, `${label}: footer reserves its footprint`);
      for (const target of metrics.targets) assert.ok(target.height >= 44 && target.width >= 44 && (target.navigation || (target.left >= -1 && target.right <= width + 1)), `${label}: visible 44px header target ${target.label} (${target.width} × ${target.height})`);
      for (const link of await page.locator('.hraness-marketing-header nav a').all()) {
        await link.scrollIntoViewIfNeeded();
        const box = await link.boundingBox();
        assert.ok(box.x >= -1 && box.x + box.width <= width + 1, `${label}: every navigation link can be brought into view`);
      }
      await page.evaluate(() => scrollTo({ top: 500, behavior: 'instant' }));
      const moved = await page.evaluate(() => ({ scroll: scrollY, header: document.querySelector('.hraness-marketing-header').getBoundingClientRect().top, footer: document.querySelector('#hraness-site-footer').getBoundingClientRect().top }));
      assert.ok(Math.abs(moved.header) <= 1, `${label}: sticky chrome`);
      assert.ok(Math.abs(moved.footer + moved.scroll - metrics.footer.top) <= 2, `${label}: footer scrolls with document`);
      records.push({ route: path, width, theme, status: response.status(), metrics });
    }
    await page.goto(origin, { waitUntil: 'load' });
    const trigger = page.locator('.hraness-design-palette-menu > summary');
    await until(() => page.locator('.hraness-design-palette-menu').getAttribute('data-ready').then(value => value === 'true'), 'hydrated appearance control');
    await trigger.click();
    await page.getByRole('radio', { name: 'Dark', exact: true }).check();
    await until(() => page.evaluate(() => document.documentElement.dataset.theme).then(value => value === 'dark'), 'dark appearance applied');
    await page.locator('.hraness-marketing-header__nav a[href="/docs"]').click();
    await page.waitForURL(origin + '/docs');
    await until(() => page.evaluate(() => document.documentElement.dataset.theme).then(value => value === 'dark'), 'appearance persists across real navigation');
    const appearanceMenu = page.locator('.hraness-design-palette-menu');
    if (await appearanceMenu.evaluate(element => element.open)) await page.keyboard.press('Escape');
    await trigger.click();
    assert.equal(await appearanceMenu.evaluate(element => element.open), true, 'appearance menu opens');
    await page.keyboard.press('Escape');
    assert.equal(await appearanceMenu.evaluate(element => element.open), false, 'Escape closes appearance menu');
    await until(() => trigger.evaluate(element => document.activeElement === element), 'appearance Escape returns focus');
    await page.locator('.hraness-marketing-header a[href="/#install"]').click();
    await page.waitForURL(origin + '/#install');
    await until(() => page.locator('#install').evaluate(element => element.getBoundingClientRect().top >= document.querySelector('.hraness-marketing-header').getBoundingClientRect().bottom - 1), 'install anchor clears sticky chrome');
    await context.close();
  }
  assert.deepEqual(errors, [], 'No browser runtime or resource errors');
  const receipt = { origin, production: values.production, sourceSha: process.env.GITHUB_SHA ?? null, startedAt: new Date(startedAt).toISOString(), completedAt: new Date().toISOString(), durationMs: Date.now() - startedAt, browserErrors: errors, pagesChecked: records.length, records };
  await writeFile(resolve(artifacts, 'verification.json'), JSON.stringify(receipt, null, 2));
  console.log(JSON.stringify({ ...receipt, records: undefined, artifacts }, null, 2));
} catch (error) {
  if (activePage && !activePage.isClosed()) await activePage.screenshot({ path: resolve(artifacts, 'failure.png'), fullPage: true }).catch(() => {});
  await writeFile(resolve(artifacts, 'failure.json'), JSON.stringify({ message: String(error), origin, errors, records }, null, 2));
  throw error;
} finally {
  clearTimeout(deadline);
  await cleanup();
}
