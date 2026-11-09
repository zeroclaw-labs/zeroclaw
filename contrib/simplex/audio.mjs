// 16 kHz mono PCM from the virtual microphone. Bounded, in-memory utterances;
// raw call audio is never saved to disk.
export function wav(pcm) {
  const header = Buffer.alloc(44);
  header.write('RIFF'); header.writeUInt32LE(36 + pcm.length, 4); header.write('WAVEfmt ', 8);
  header.writeUInt32LE(16, 16); header.writeUInt16LE(1, 20); header.writeUInt16LE(1, 22);
  header.writeUInt32LE(16000, 24); header.writeUInt32LE(32000, 28);
  header.writeUInt16LE(2, 32); header.writeUInt16LE(16, 34);
  header.write('data', 36); header.writeUInt32LE(pcm.length, 40);
  return Buffer.concat([header, pcm]);
}

export class Utterances {
  constructor(onStart, onEnd) {
    this.onStart = onStart; this.onEnd = onEnd;
    this.reset();
  }
  reset() {this.parts = []; this.samples = 0; this.silence = 0; this.voiced = 0; this.active = false; this.pre = [];}
  push(pcm) {
    if (pcm.length % 2 || pcm.length > 8192) throw Error('invalid PCM frame');
    let sum = 0;
    for (let i = 0; i < pcm.length; i += 2) sum += (pcm.readInt16LE(i) / 32768) ** 2;
    const count = pcm.length / 2;
    const speech = count > 0 && Math.sqrt(sum / count) >= 0.012;
    if (!this.active) {
      this.pre.push(pcm); if (this.pre.length > 4) this.pre.shift();
      this.voiced = speech ? this.voiced + count : 0;
      if (this.voiced < 1600) return;
      this.active = true; this.parts = this.pre; this.pre = [];
      this.samples = this.parts.reduce((n, b) => n + b.length / 2, 0);
      this.onStart();
      return;
    }
    this.parts.push(pcm); this.samples += count;
    this.silence = speech ? 0 : this.silence + count;
    if (this.silence >= 11200) {
      const audio = wav(Buffer.concat(this.parts));
      this.reset(); this.onEnd(audio, true);
    } else if (this.samples >= 480000) {
      const audio = wav(Buffer.concat(this.parts));
      this.parts = []; this.samples = 0;
      // Keep the same voice turn and generation while STT works on this chunk.
      this.onEnd(audio, false);
    }
  }
}
