import assert from 'node:assert/strict';
import { publicationLinkGroups, verifyPublicationLinks } from './verify-publication-links.mjs';
import { verifySettledConsentFlow } from './verify-settled-consent.mjs';
import { inspectMockupLayout } from './check-mockup-layout.mjs';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdir, writeFile } from 'node:fs/promises';
import { createServer } from 'node:net';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { chromium } from 'playwright-core';
import { browserOwner, localVerificationOrigin, ownedChromiumLaunchOptions, pinnedBrowserExecutable, pinnedChromiumDefinition, verifyOwnedChromium } from './owned-browser.mjs';

const { values } = parseArgs({ options: { production: { type: 'boolean', default: false }, 'local-origin': { type: 'string' } }, strict: true });
assert.equal(process.argv.slice(2).filter(argument => argument === '--local-origin' || argument.startsWith('--local-origin=')).length, Number(values['local-origin'] !== undefined), 'Provide at most one local origin.');
const localOrigin = localVerificationOrigin(values['local-origin'], values.production);
const repository = resolve(import.meta.dirname, '..');
const artifacts = resolve(repository, '.impeccable/review', `public-${Date.now()}`);
await mkdir(artifacts, { recursive: true });
const routes = ['/', '/docs', '/benchmarks', '/methodology', '/compare/claude-code-compact', '/compare/cliffcompaction', '/blog', '/blog/introducing-gobstopper', '/blog/gobstopper-on-terminal-bench', '/blog/proofs-for-the-admission-math', '/blog/vault-models-that-fail-on-purpose', '/missing-public-verification'];
const anchors = ['/#terminal-bench', '/benchmarks#terminal-bench-2026-09-28'];
const errors = [];
const records = [];
const startedAt = Date.now();
let server;
let exited;
let browser;
let launchOptions;
let browserIdentity;
const activePages = new Map();
const CONTEXT_POOL = Math.max(1, Number.parseInt(process.env.GOBSTOPPER_BROWSER_CONTEXTS ?? '3', 10) || 3);
let cleanupPromise;
let interruption;
let origin = localOrigin ?? 'https://gobstopper.sh';
const pause = (ms) => new Promise(resolve => setTimeout(resolve, ms));
async function until(check, label, timeout = 5000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    if (interruption) throw interruption;
    if (await check()) return;
    await pause(50);
  }
  throw new Error(`Timed out: ${label}`);
}
async function stopServer() {
  if (!server) return;
  if (server.exitCode === null && server.signalCode === null) server.kill('SIGTERM');
  await Promise.race([exited, pause(5000)]);
  if (server.exitCode === null && server.signalCode === null) { server.kill('SIGKILL'); await exited; }
}
const owner = browserOwner({
  launch: () => chromium.launch({ ...launchOptions, timeout: 15000,
    handleSIGHUP: false, handleSIGINT: false, handleSIGTERM: false }),
  close: async active => { await active.close(); }, stopServer,
});
function cleanup() { return cleanupPromise ??= owner.stop(); }
function interrupt(error, code) {
  interruption ??= error;
  process.exitCode = code;
  void cleanup().catch(error => { console.error(error); process.exitCode = 1; });
}
const deadline = setTimeout(() => {
  const error = new Error('Public browser verification exceeded four minutes.');
  console.error(error.message);
  interrupt(error, 1);
}, 240000);
for (const [signal, code] of [['SIGHUP', 129], ['SIGINT', 130], ['SIGTERM', 143]]) {
  process.once(signal, () => interrupt(new Error(`Browser verification interrupted by ${signal}.`), code));
}
try {
  const definition = pinnedChromiumDefinition();
  const executablePath = await pinnedBrowserExecutable(chromium.executablePath(), process.env.GOBSTOPPER_BROWSER_EXECUTABLE);
  launchOptions = ownedChromiumLaunchOptions(executablePath, definition.defaultArgs);
  if (interruption) throw interruption;
  if (!values.production && !localOrigin) {
    const socket = createServer();
    socket.listen(0, '127.0.0.1'); await once(socket, 'listening');
    const port = socket.address().port;
    await new Promise(resolve => socket.close(resolve));
    if (interruption) throw interruption;
    origin = `http://127.0.0.1:${port}`;
    server = spawn(process.execPath, [resolve(repository, 'node_modules/next/dist/bin/next'), 'start', '--hostname', '127.0.0.1', '--port', String(port)], {
      cwd: repository, stdio: ['ignore', 'inherit', 'inherit'], env: process.env,
    });
    exited = new Promise((resolveExit, reject) => { server.once('exit', resolveExit); server.once('error', reject); });
    void exited.catch(() => undefined);
    server.once('error', error => errors.push(`Server: ${error.message}`));
    await until(async () => { assert.equal(server.exitCode, null, 'Owned Next server exited'); return fetch(origin, { signal: AbortSignal.timeout(1000) }).then(r => r.ok, () => false); }, 'Next production server', 30000);
  }
  browser = await owner.start();
  browserIdentity = await verifyOwnedChromium(browser, executablePath, definition.expectedVersion);
  // Eight (width, theme) contexts share one browser; a small pool keeps the
  // run short without starving the one CI runner. Each context has its own
  // page, records and failure screenshot; results are merged in fixed order.
  const combos = [320, 360, 390, 1440].flatMap(width => ['light', 'dark'].map(theme => ({ width, theme })));
  const comboRecords = combos.map(() => []);
  const failures = [];
  async function checkCombo({ width, theme }, comboRecords, index) {
    const context = await browser.newContext({ viewport: { width, height: width === 360 ? 740 : width === 390 ? 844 : 900 }, colorScheme: theme, reducedMotion: 'reduce', serviceWorkers: 'block' });
    const page = await context.newPage(); activePages.set(index, page);
    page.setDefaultTimeout(10000);
    page.on('pageerror', error => errors.push(`${width}-${theme}: ${error.message}`));
    page.on('console', message => { if (message.type() === 'error' && !message.location().url.includes('/missing-public-verification')) errors.push(`${width}-${theme}: ${message.text()}`); });
    page.on('response', response => { if (response.status() >= 400 && !response.url().includes('/missing-public-verification')) errors.push(`${width}-${theme}: HTTP ${response.status()} ${new URL(response.url()).pathname}`); });
    for (const path of routes) {
      const label = `${path === '/' ? 'home' : path.slice(1).replaceAll('/', '-')}-${width}-${theme}`;
      const response = await page.goto(origin + path, { waitUntil: 'load' });
      assert.equal(response.status(), path === '/missing-public-verification' ? 404 : 200, label);
      await page.evaluate(async () => { await document.fonts.ready; });
      await page.locator('img').evaluateAll(images => Promise.all(images.map(image => { image.loading = 'eager'; return image.decode(); })));
      assert.match(await page.title(), /Gobstopper/i, label);
      assert.equal(await page.locator('h1').count(), 1, label);
      assert.equal(await page.locator('#hraness-site-footer').count(), 1, label);
      assert.equal(await page.locator('iframe').count(), 0, 'Retired embedded preview stays absent');
      await until(() => page.evaluate(() => document.documentElement.dataset.theme).then(value => value === theme), `${label}: resolved theme`);
      const settledConsent = path === '/' ? await verifySettledConsentFlow(page) : undefined;
      const publicationLinks = path === '/blog/introducing-gobstopper'
        ? await verifyPublicationLinks(page, [
            ...publicationLinkGroups.map(group => ({ ...group, required: group.name !== 'footer' })),
            { name: 'byline', selector: '.plain-publication__byline a[href]', required: true },
          ])
        : undefined;
      const screenshot = await page.screenshot({ path: resolve(artifacts, `${label}.png`), fullPage: true, animations: 'disabled' });
      assert.equal(screenshot.readUInt32BE(16), width, `${label}: full-page screenshot width`);
      const metrics = await page.evaluate(() => {
        const header = document.querySelector('.hraness-marketing-header');
        const footer = document.querySelector('#hraness-site-footer');
        const inner = footer.querySelector('.hraness-site-footer__inner');
        const main = document.querySelector('main');
        const rect = element => { const r = element.getBoundingClientRect(); return { top: r.top, bottom: r.bottom, left: r.left, right: r.right, height: r.height, width: r.width }; };
        return { overflow: document.documentElement.scrollWidth - innerWidth, bodyOverflow: document.body.scrollWidth - innerWidth, bodyWidth: document.body.getBoundingClientRect().width, header: rect(header), headerPosition: getComputedStyle(header).position, footer: rect(footer), footerInner: rect(inner), footerPosition: getComputedStyle(inner).position, main: rect(main), font: getComputedStyle(document.body).fontFamily, targets: [...header.querySelectorAll('a, button, summary')].map(a => ({ label: a.getAttribute('href') ?? a.getAttribute('aria-label'), navigation: Boolean(a.closest('nav')), ...rect(a) })) };
      });
      assert.ok(metrics.overflow <= 1, `${label}: document overflow`);
      assert.ok(metrics.bodyOverflow <= 1 && metrics.bodyWidth <= width + 1, `${label}: body overflow`);
      assert.match(metrics.font, /Nebula Sans/, label);
      assert.equal(metrics.headerPosition, 'sticky', label);
      assert.ok(metrics.header.height <= (width < 600 ? 140 : 90), `${label}: header height`);
      assert.ok(['static', 'relative'].includes(metrics.footerPosition), `${label}: footer in flow`);
      assert.ok(metrics.footer.top >= metrics.main.bottom - 1, `${label}: footer follows main`);
      assert.ok(metrics.footer.height >= metrics.footerInner.height - 1, `${label}: footer reserves its footprint`);
      const undersizedTargets = metrics.targets.filter(target => target.height < 44 || target.width < 44 || (!target.navigation && (target.left < -1 || target.right > width + 1)));
      assert.deepEqual(undersizedTargets, [], `${label}: all header targets must be visible and at least 44px: ${JSON.stringify(undersizedTargets)}`);
      for (const link of await page.locator('.hraness-marketing-header nav a').all()) {
        await link.scrollIntoViewIfNeeded();
        const box = await link.boundingBox();
        assert.ok(box.x >= -1 && box.x + box.width <= width + 1, `${label}: every navigation link can be brought into view`);
      }
      await page.evaluate(() => scrollTo({ top: 500, behavior: 'instant' }));
      const moved = await page.evaluate(() => ({ scroll: scrollY, header: document.querySelector('.hraness-marketing-header').getBoundingClientRect().top, footer: document.querySelector('#hraness-site-footer').getBoundingClientRect().top }));
      assert.ok(Math.abs(moved.header) <= 1, `${label}: sticky chrome`);
      assert.ok(Math.abs(moved.footer + moved.scroll - metrics.footer.top) <= 2, `${label}: footer scrolls with document`);
      const mockups = ['/', '/blog/introducing-gobstopper'].includes(path) ? await inspectMockupLayout(page, width === 320 || width === 390) : undefined;
      comboRecords.push({ route: path, width, theme, status: response.status(), metrics, mockups, publicationLinks, settledConsent });
      const figures = await page.evaluate(() => [...document.querySelectorAll('.gob-figure')].map(figure => {
        const box = figure.getBoundingClientRect();
        const small = [...figure.querySelectorAll('*')].filter(element => element.childNodes.length > 0 && [...element.childNodes].some(node => node.nodeType === 3 && node.textContent.trim() !== '') && element.checkVisibility() && parseFloat(getComputedStyle(element).fontSize) < 11.5).map(element => element.textContent.trim().slice(0, 40));
        const escaped = [...figure.querySelectorAll('.gob-figure__plot *')].filter(element => element.checkVisibility() && getComputedStyle(element).position !== 'static').filter(element => { const r = element.getBoundingClientRect(); return r.width > 0 && (r.left < box.left - 1 || r.right > box.right + 1); }).map(element => element.className);
        return { id: figure.id, left: box.left, right: box.right, small, escaped };
      }));
      for (const figure of figures) {
        assert.ok(figure.left >= -1 && figure.right <= width + 1, `${label}: ${figure.id} fits the viewport`);
        assert.deepEqual(figure.small, [], `${label}: ${figure.id} text is at least 12px`);
        assert.deepEqual(figure.escaped, [], `${label}: ${figure.id} marks stay inside the frame`);
      }
    }
    for (const anchor of anchors) {
      const label = `anchor-${anchor.replace(/[^a-z0-9]+/gu, '-').replace(/^-|-$/gu, '')}-${width}-${theme}`;
      const response = await page.goto(origin + anchor, { waitUntil: 'load' });
      assert.equal(response.status(), 200, label);
      const id = anchor.split('#')[1];
      assert.equal(await page.locator(`#${id}`).count(), 1, `${label}: anchor target exists`);
      await until(() => page.locator(`#${id}`).evaluate(element => element.getBoundingClientRect().top >= document.querySelector('.hraness-marketing-header').getBoundingClientRect().bottom - 1), `${label}: anchor clears sticky chrome`);
      await page.screenshot({ path: resolve(artifacts, `${label}.png`), animations: 'disabled' });
      for (const [index, figure] of (await page.locator('.gob-figure').all()).entries()) {
        await figure.screenshot({ path: resolve(artifacts, `${label}-figure-${index}.png`), animations: 'disabled' });
      }
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
    await context.close(); activePages.delete(index);
  }
  let next = 0;
  await Promise.all(Array.from({ length: Math.min(CONTEXT_POOL, combos.length) }, async () => {
    while (next < combos.length) {
      const index = next++;
      const { width, theme } = combos[index];
      try { await checkCombo(combos[index], comboRecords[index], index); } catch (error) {
        const page = activePages.get(index);
        if (page && !page.isClosed()) await page.screenshot({ path: resolve(artifacts, `failure-${width}-${theme}.png`), fullPage: true }).catch(() => {});
        failures.push(`${width}-${theme}: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
  }));
  records.push(...comboRecords.flat());
  if (failures.length) throw new Error(`Public browser checks failed:\n${failures.sort().join('\n')}`);
  assert.deepEqual(errors, [], 'No browser runtime or resource errors');
  const receipt = { browserExecutable: browserIdentity.executable, browserVersion: browserIdentity.browserVersion, browserIdentity, origin, production: values.production, sourceSha: process.env.GITHUB_SHA ?? null, startedAt: new Date(startedAt).toISOString(), completedAt: new Date().toISOString(), durationMs: Date.now() - startedAt, browserErrors: errors, pagesChecked: records.length, records };
  await writeFile(resolve(artifacts, 'verification.json'), JSON.stringify(receipt, null, 2));
  console.log(JSON.stringify({ ...receipt, records: undefined, artifacts }, null, 2));
} catch (error) {
  for (const [index, page] of activePages) if (!page.isClosed()) await page.screenshot({ path: resolve(artifacts, `failure-context-${index}.png`), fullPage: true }).catch(() => {});
  await writeFile(resolve(artifacts, 'failure.json'), JSON.stringify({ message: String(error), origin, browserIdentity, errors, records }, null, 2));
  throw error;
} finally {
  clearTimeout(deadline);
  await cleanup();
}
