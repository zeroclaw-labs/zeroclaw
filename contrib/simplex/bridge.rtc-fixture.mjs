import {access, appendFile} from 'node:fs/promises';

const logPath = process.env.SIMPLEX_TEST_RTC_LOG;
const releasePath = process.env.SIMPLEX_TEST_RTC_RELEASE;
if (!logPath || !releasePath) throw Error('bridge_rtc_fixture_not_configured');

const record = value => appendFile(logPath, `${value}\n`);
const pause = () => new Promise(resolve => {
  const timer = setTimeout(resolve, 10);
  timer.unref();
});

async function released() {
  try {
    await access(releasePath);
    return true;
  } catch (error) {
    if (error.code === 'ENOENT') return false;
    throw error;
  }
}

export async function openRtc({onSignal}) {
  await record('open');
  while (!await released()) await pause();
  // The browser can emit a connection update while opening. Once hangup has
  // abandoned this RTC, that update must not reach the SimpleX CLI.
  await record('signal');
  await onSignal({resp: {type: 'connection', state: {connectionState: 'connected'}}});
  return {
    command: async command => {
      await record(`command:${command.type}`);
      return {resp: {type: 'offer', capabilities: {encryption: true}, offer: 'stale', iceCandidates: []}};
    },
    play: async () => record('play'),
    stop: async () => {
      await record('stop');
      return null;
    },
    close: () => record('close'),
  };
}
