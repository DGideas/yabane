// This is deliberately a white-box console test: observe the loaders invoked by
// navigation, not fetch() (which resolves at headers) or a guessed rendering delay.
// Keep this map in sync with showView; loader promises cover JSON parsing + render.
const viewLoaders = {
  home: ['loadDashboard'],
  providers: ['loadProviderActivity'],
  activity: ['loadActivity', 'loadActivityPage'], // The overview can start an unawaited page load.
  management: ['loadManagementKeys'],
  models: [], pricing: [], extensions: [], access: [],
};

export async function openConsoleView(page, view, {timeout = 5000} = {}) {
  const loaders = viewLoaders[view];
  if (!loaders) throw new Error(`No readiness contract for console view: ${view}`);
  try {
    await page.evaluate(({view, loaders}) => {
      const state = {pending: 0, called: [], error: null, originals: {}};
      window.__yabaneViewLoad = state;
      for (const name of loaders) {
        const original = window[name];
        if (typeof original !== 'function') throw new Error(`Missing console loader: ${name}`);
        state.originals[name] = original;
        window[name] = function (...args) {
          state.called.push(name);
          state.pending += 1;
          const failed = error => {
            state.error = String(error);
            state.pending -= 1;
          };
          try {
            const result = original.apply(this, args);
            if (!result || typeof result.then !== 'function') throw new Error(`${name} must return its load/render promise`);
            result.then(() => { state.pending -= 1; }, failed);
            return result;
          } catch (error) {
            failed(error);
            throw error;
          }
        };
      }
      const navigation = document.querySelector(`#console-sidebar [data-view="${view}"]`);
      if (!navigation) throw new Error(`Missing console navigation: ${view}`);
      navigation.click();
      // Do not call a loader ourselves: that would hide broken navigation.
      if (loaders.length && !state.called.includes(loaders[0])) {
        throw new Error(`Navigation did not invoke ${loaders[0]}`);
      }
    }, {view, loaders});
    await page.waitForFunction(() => {
      const state = window.__yabaneViewLoad;
      if (state.error !== null) throw new Error(`Console view failed to load: ${state.error}`);
      return state.pending === 0;
    }, undefined, {timeout});
    await page.locator(`#${view}-view`).waitFor({state: 'visible', timeout});
    // The mobile sidebar can be closed; its active navigation entry must still exist.
    await page.locator(`#console-sidebar .nav.active[data-view="${view}"]`).waitFor({state: 'attached', timeout});
    const renderedContent = {
      home: '#home-updated', providers: '#provider-list-page',
      activity: '#recent-activity-logs tr',
      management: '#management-keys-empty:not([hidden]), #management-keys-table:not([hidden])',
    }[view];
    if (renderedContent) await page.locator(renderedContent).first().waitFor({state: 'attached', timeout});
  } finally {
    await page.evaluate(() => {
      const state = window.__yabaneViewLoad;
      if (!state) return;
      for (const [name, original] of Object.entries(state.originals)) window[name] = original;
      delete window.__yabaneViewLoad;
    });
  }
}
