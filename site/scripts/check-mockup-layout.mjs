import assert from 'node:assert/strict';

/** Exercise every authored state, including combinations absent from the first render. */
export async function inspectMockupLayout(page, withZoom = false) {
  await page.evaluate(() => document.fonts.ready);
  const samples = [];
  const showcases = page.locator('.hkm-showcase[data-hkm-fit="fill"]');
  for (let index = 0; index < await showcases.count(); index++) {
    const showcase = showcases.nth(index);
    const steps = await showcase.evaluate(element => element.classList.contains('hkm-steps'));
    await showcase.locator(steps ? '.hkm-step-stage[data-hkm-fitted]' : '.hkm-mode-stage[data-hkm-fitted]').waitFor();
    const groups = showcase.locator('.hkm-segmented');
    const primary = steps ? showcase.getByRole('tab') : groups.first().getByRole('button');
    const options = !steps && await groups.count() > 1 ? groups.nth(1).getByRole('button') : null;
    const states = [];
    // Inspect the first visible state before interacting, then every state that
    // was initially hidden. This catches sizing that changes only on first open.
    for (let choice = -1; choice < await primary.count(); choice++) {
      if (choice >= 0) await primary.nth(choice).click();
      for (let option = 0; option < (choice < 0 ? 1 : options ? await options.count() : 1); option++) {
        if (choice >= 0 && options) await options.nth(option).click();
        await page.evaluate(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))));
        const state = await showcase.evaluate(figure => {
          const stage = figure.querySelector('.hkm-step-stage, .hkm-mode-stage');
          const active = stage.querySelector('[aria-hidden="false"]');
          const frame = active.querySelector('.hkm-window');
          const terminal = active.querySelector('[data-hkm-density="presentation"]');
          const agent = active.querySelector('.hkm-agent-body');
          const lastTurn = agent?.querySelector('.hkm-agent-turn:last-child');
          const visibleBottom = element => {
            let bottom = Math.min(element.getBoundingClientRect().bottom, frame.getBoundingClientRect().bottom, stage.getBoundingClientRect().bottom);
            for (let current = element; current && stage.contains(current); current = current.parentElement) {
              const css = getComputedStyle(current);
              if (/hidden|clip|auto|scroll/.test(`${css.overflow} ${css.overflowY}`)) bottom = Math.min(bottom, current.getBoundingClientRect().bottom);
              const fade = getComputedStyle(current, '::after');
              if ((current.hasAttribute('data-hkm-fade') || current.classList.contains('hkm-scroll-fade')) && fade.display !== 'none' && fade.content !== 'none') bottom = Math.min(bottom, current.getBoundingClientRect().bottom - (parseFloat(fade.blockSize) || 0));
            }
            return bottom;
          };
          const terminalBottom = terminal && visibleBottom(terminal);
          const clippedLines = terminal && [...terminal.querySelectorAll('.hkm-terminal-line')].map((line, index) => ({ index, bottom: line.getBoundingClientRect().bottom })).filter(line => line.bottom > terminalBottom + 1);
          return { height: stage.getBoundingClientRect().height, width: stage.getBoundingClientRect().width,
            frameWidth: frame.getBoundingClientRect().width, frameHeight: frame.getBoundingClientRect().height,
            scaled: active.querySelector('[data-hkm-scaled]') !== null,
            terminal: terminal && { visibleBottom: terminalBottom, clippedLines, size: parseFloat(getComputedStyle(terminal).fontSize), minimum: parseFloat(getComputedStyle(document.documentElement).fontSize),
              overflow: terminal.scrollHeight > terminal.clientHeight + 1 || terminal.scrollWidth > terminal.clientWidth + 1 },
            agent: agent && { overflow: agent.scrollHeight > agent.clientHeight + 1 || agent.scrollWidth > agent.clientWidth + 1,
              lastTurnVisible: lastTurn && lastTurn.getBoundingClientRect().bottom <= visibleBottom(agent) + 1 },
            chart: active.querySelector('.gob-meter__chart') && { width: active.querySelector('.gob-meter__chart').getBoundingClientRect().width,
              height: active.querySelector('.gob-meter__chart').getBoundingClientRect().height } };
        });
        assert.ok(!state.scaled, 'Mockup text must reflow without scaling');
        assert.ok(state.frameWidth > 1 && state.frameWidth <= state.width + 1, 'Mockup frame fits its column');
        assert.ok(Math.abs(state.frameHeight - state.height) <= 3, 'Every frame fills its stable stage');
        if (state.terminal) assert.ok(state.terminal.size >= state.terminal.minimum - 0.1 && !state.terminal.overflow && state.terminal.clippedLines.length === 0, `Complete terminal respects reader text size: ${JSON.stringify({ index, choice, option, ...state })}`);
        if (state.agent) assert.ok(!state.agent.overflow && state.agent.lastTurnVisible, `Complete exchange fits inside the agent frame: ${JSON.stringify({ index, choice, option, ...state })}`);
        if (state.chart) assert.ok(state.chart.width > 1 && state.chart.height > 1, 'Recorded meter chart remains visible');
        states.push({ choice, option, ...state });
      }
    }
    assert.ok(Math.max(...states.map(state => state.height)) - Math.min(...states.map(state => state.height)) <= 1, 'Every mode and option reserves one stable height');
    samples.push({ index, steps, states });
  }
  assert.equal(samples.length, new URL(page.url()).pathname === '/' ? 2 : 3, 'Exercise all walkthroughs on the page');
  let zoom;
  if (withZoom) {
    const fontSize = await page.evaluate(() => document.documentElement.style.fontSize);
    try {
      await page.evaluate(() => { document.documentElement.style.fontSize = '200%'; });
      zoom = await inspectMockupLayout(page);
      assert.ok(await page.evaluate(() => Math.max(document.documentElement.scrollWidth, document.body.scrollWidth) <= innerWidth + 1), '200% text fits the phone viewport');
    } finally { await page.evaluate(value => { document.documentElement.style.fontSize = value; }, fontSize); }
  }
  return { samples, ...(zoom ? { zoom } : {}) };
}
