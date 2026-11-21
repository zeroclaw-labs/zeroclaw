// SimpleX's separately installed, unmodified WebRTC assets own encryption and
// SDP compatibility. This module supplies virtual audio, never a physical mic.
import {chromium} from 'playwright-core';
import {readFile} from 'node:fs/promises';
import {join} from 'node:path';

export async function openRtc({assets, executablePath, onSignal, onPcm}) {
  const browser = await chromium.launch({executablePath, headless: true,
    args: ['--autoplay-policy=no-user-gesture-required']});
  try {
    const page = await browser.newPage();
    // No HTTP listener and no external scripts. Media is virtual; getUserMedia
    // is replaced before the upstream implementation can request permissions.
    await page.route('**/*', route => route.request().url() === 'http://127.0.0.1/simplex-call'
      ? route.fulfill({contentType: 'text/html', body: '<video id="local-video-stream" muted></video><video id="local-screen-video-stream" muted></video><video id="remote-video-stream" muted></video><video id="remote-screen-video-stream" muted></video>'})
      : route.abort());
    await page.goto('http://127.0.0.1/simplex-call');
    await page.exposeBinding('hostSignal', (_source, signal) => onSignal(signal));
    await page.exposeBinding('hostPcm', (_source, pcm) => onPcm(pcm));
    await page.evaluate(() => {
      // Playback preserves the TTS source bandwidth. Recognition has its own
      // 16 kHz clock, which owns the PCM/VAD timing contract with the host.
      const audio = new AudioContext({sampleRate: 48000});
      const recognition = new AudioContext({sampleRate: 16000});
      const destination = audio.createMediaStreamDestination();
      destination.channelCount = 1;
      // This is synthesized audio, not a noisy microphone. Prefer fidelity over
      // speech enhancement that can discard breath sounds and high frequencies.
      destination.stream.getAudioTracks()[0].contentHint = 'music';
      const silence = audio.createConstantSource();
      silence.offset.value = 0;
      silence.connect(destination);
      silence.start();
      // Owns scheduled clip receipts until the next stop. Ended clips release
      // their AudioBufferSourceNode; only bounded text/timing remains.
      const sources = new Set();
      const waiting = new Set();
      let next = 0;
      globalThis.playAudio = async (base64, metadata = {}) => {
        let cancelled = false, timer, rejectStarted, starts;
        const cancel = () => {
          cancelled = true;
          clearTimeout(timer);
          rejectStarted?.(Object.assign(Error('simplex_audio_cancelled'),
            {notDelivered: starts === undefined || audio.currentTime < starts}));
        };
        // The existing waiting set owns cancellation from decode admission
        // through playback start. A stop also invalidates a pending decode.
        waiting.add(cancel);
        try {
          const raw = Uint8Array.from(atob(base64), c => c.charCodeAt(0));
          const buffer = await audio.decodeAudioData(raw.buffer);
          if (cancelled) throw Object.assign(Error('simplex_audio_cancelled'), {notDelivered: true});
          if (buffer.duration > 180 || [...sources].filter(clip => clip.source).length >= 2 || sources.size >= 256 || [...sources].reduce((n, clip) => n + clip.text.length, 0) + (metadata.text?.length ?? 0) > 262144) throw Object.assign(Error('simplex_audio_queue_limit'), {notDelivered: true});
          const source = audio.createBufferSource();
          source.buffer = buffer;
          source.connect(destination);
          next = Math.max(next, audio.currentTime + 0.025);
          starts = next;
          source.start(starts);
          next += buffer.duration;
          const clip = {source, text: metadata.text ?? '', scope: metadata.scope ?? '',
            segment: metadata.segment ?? '', starts, ends: starts + buffer.duration};
          sources.add(clip);
          source.onended = () => {clip.source = null;};
          // ACK at playback start, allowing one future clip while the active
          // clip plays. Already audible delivery is not retroactively rejected.
          await new Promise((resolve, reject) => {
            rejectStarted = reject;
            timer = setTimeout(resolve, Math.max(0, starts - audio.currentTime) * 1000);
          });
        } finally {waiting.delete(cancel);}
      };
      globalThis.stopAudio = () => {
        const now = audio.currentTime;
        const completed = [], partials = [];
        let queued = 0;
        for (const clip of sources) {
          if (now >= clip.ends) completed.push(clip.text);
          else if (now <= clip.starts) queued++;
          else partials.push({text: clip.text, scope: clip.scope, segment: clip.segment,
            played_ms: Math.floor((now - clip.starts) * 1000),
            duration_ms: Math.ceil((clip.ends - clip.starts) * 1000)});
        }
        const report = {interrupted: partials.length > 0 || queued > 0 || waiting.size > 0,
          completed, partial: partials[0] ?? null, queued};
        for (const clip of sources) clip.source?.stop();
        for (const cancel of waiting) cancel();
        waiting.clear();
        sources.clear(); next = 0;
        return report;
      };
      Object.defineProperty(navigator, 'mediaDevices', {value: {
        getUserMedia: async () => destination.stream,
        enumerateDevices: async () => [{kind: 'audioinput', deviceId: 'virtual'}],
      }});
      const Original = RTCPeerConnection;
      globalThis.RTCPeerConnection = class extends Original {
        constructor(...args) {
          super(...args);
          this.addEventListener('track', event => {
            // SimpleX also negotiates a separate screen-audio transceiver.
            // Mixing its silence with the mic would repeatedly reset VAD.
            if (event.track.kind !== 'audio' || event.transceiver.mid !== '0') return;
            const input = recognition.createMediaStreamSource(new MediaStream([event.track]));
            const processor = recognition.createScriptProcessor(1024, 1, 1);
            input.connect(processor); processor.connect(recognition.destination);
            const frames = [];
            let pumping = false, failed = false;
            const fail = () => {
              if (failed) return;
              failed = true; frames.length = 0; processor.onaudioprocess = null;
              hostSignal({resp: {type: 'audio_error'}});
            };
            const drain = async () => {
              pumping = true;
              try {while (frames.length && !failed) await hostPcm(frames.shift());}
              catch {fail();}
              finally {pumping = false;}
            };
            processor.onaudioprocess = event => {
              if (failed) return;
              if (frames.length >= 16) {fail(); return;}
              const data = event.inputBuffer.getChannelData(0);
              const bytes = new Uint8Array(data.length * 2);
              const view = new DataView(bytes.buffer);
              for (let i = 0; i < data.length; i++) view.setInt16(i * 2, Math.max(-32768, Math.min(32767, data[i] * 32767)), true);
              frames.push(btoa(String.fromCharCode(...bytes)));
              if (!pumping) drain();
            };
          });
        }
      };
      audio.resume(); recognition.resume();
      globalThis.audioState = () => audio.state;
      globalThis.audioDiagnostics = async spectrum => {
        const result = {playbackSampleRate: audio.sampleRate, recognitionSampleRate: recognition.sampleRate,
          contentHint: destination.stream.getAudioTracks()[0].contentHint,
          playingSources: [...sources].filter(clip => clip.source).length, pendingPlaybacks: waiting.size,
          track: {sampleRate: destination.stream.getAudioTracks()[0].getSettings().sampleRate}};
        const remote = typeof activeCall === 'undefined' ? undefined
          : activeCall?.connection.getTransceivers().find(t => t.mid === '0')?.receiver.track;
        if (!spectrum || !remote) return result;
        // Materialize only aggregate diagnostics from the canonical live track.
        // No samples or long-lived analysis cache are retained.
        const input = audio.createMediaStreamSource(new MediaStream([remote]));
        const analyser = audio.createAnalyser(); analyser.fftSize = 2048; analyser.smoothingTimeConstant = 0;
        const mute = audio.createGain(); mute.gain.value = 0;
        try {
          input.connect(analyser); analyser.connect(mute); mute.connect(audio.destination);
          await new Promise(resolve => setTimeout(resolve, 150));
          const bins = new Float32Array(analyser.frequencyBinCount); analyser.getFloatFrequencyData(bins);
          const first = Math.ceil(9000 * analyser.fftSize / audio.sampleRate);
          const last = Math.floor(11000 * analyser.fftSize / audio.sampleRate);
          result.highBandPeakDb = Math.max(...bins.slice(first, last + 1));
          result.overallPeakDb = Math.max(...bins);
          result.peakFrequencyHz = bins.indexOf(result.overallPeakDb) * audio.sampleRate / analyser.fftSize;
          input.disconnect();
          const local = audio.createMediaStreamSource(destination.stream);
          try {
            local.connect(analyser);
            await new Promise(resolve => setTimeout(resolve, 150));
            analyser.getFloatFrequencyData(bins);
            result.localHighBandPeakDb = Math.max(...bins.slice(first, last + 1));
          } finally {local.disconnect();}
        } finally {input.disconnect(); analyser.disconnect(); mute.disconnect();}
        return result;
      };
    });
    for (const file of ['lz-string.min.js', 'call.js']) {
      await page.addScriptTag({content: await readFile(join(assets, file), 'utf8')});
    }
    await page.evaluate(() => {sendMessageToNative = signal => hostSignal(signal);});
    return {
      command: command => page.evaluate(async command => {
        const result = await processCommand({command});
        if (['offer', 'answer'].includes(command.type) && result.resp.type !== 'error' && activeCall) {
          for (const sender of activeCall.connection.getSenders()) {
            if (sender.track?.kind !== 'audio') continue;
            const parameters = sender.getParameters();
            if (!parameters.encodings?.length) continue;
            for (const encoding of parameters.encodings) encoding.maxBitrate = 64000;
            await sender.setParameters(parameters);
          }
        }
        return result;
      }, command),
      play: async (audio, metadata = {}) => {
        const result = await page.evaluate(async ({audio, metadata}) => {
          try {await playAudio(audio, metadata); return {ok: true};}
          catch (error) {return {ok: false, notDelivered: error.notDelivered === true};}
        }, {audio, metadata});
        if (!result.ok) throw Object.assign(Error('simplex_playback_failed'), {notDelivered: result.notDelivered});
      },
      stop: () => page.evaluate(() => stopAudio()),
      diagnostics: ({spectrum = false} = {}) => page.evaluate(async spectrum => {
        const stats = activeCall ? [...await activeCall.connection.getStats()].map(([,s]) => s) : [];
        return {audio: audioState(), media: await audioDiagnostics(spectrum),
          state: activeCall?.connection.connectionState,
          local: activeCall?.localStream.getAudioTracks().map(t => ({enabled: t.enabled, state: t.readyState})),
          codecs: stats.filter(s => s.type === 'codec' && s.mimeType?.startsWith('audio/'))
            .map(s => ({mimeType: s.mimeType, clockRate: s.clockRate, fmtp: s.sdpFmtpLine})),
          stats: stats.filter(s => ['inbound-rtp', 'outbound-rtp'].includes(s.type) && s.kind === 'audio')
            .map(s => ({type: s.type, packetsReceived: s.packetsReceived, packetsSent: s.packetsSent, audioLevel: s.audioLevel, bytesSent: s.bytesSent, targetBitrate: s.targetBitrate}))};
      }, spectrum),
      close: () => browser.close(),
    };
  } catch (error) {await browser.close(); throw error;}
}
