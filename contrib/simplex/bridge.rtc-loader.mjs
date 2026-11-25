const bridgeUrl = new URL('./bridge.mjs', import.meta.url).href;
const fixtureUrl = new URL('./bridge.rtc-fixture.mjs', import.meta.url).href;

export async function resolve(specifier, context, nextResolve) {
  if (specifier === './rtc.mjs' && context.parentURL === bridgeUrl)
    return {url: fixtureUrl, shortCircuit: true};
  return nextResolve(specifier, context);
}
