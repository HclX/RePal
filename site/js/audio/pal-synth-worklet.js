/**
 * PalRemake Stereo AudioWorkletProcessor (Issue #33).
 * Plays stereo PCM frames pushed over `port.postMessage({ l, r })` from the
 * main-thread WASM DualSynth producer, decoupling audio output from rAF/GC stalls.
 */
class PalSynthWorkletProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.cap = 32768;
    this.ringL = new Float32Array(this.cap);
    this.ringR = new Float32Array(this.cap);
    this.readIdx = 0;
    this.writeIdx = 0;
    this.available = 0;
    this.pendingFrames = 0;
    this.port.onmessage = (ev) => {
      const data = ev.data;
      if (!data) return;
      if (data.type === "clear") {
        this.readIdx = 0;
        this.writeIdx = 0;
        this.available = 0;
        this.pendingFrames = 0;
        return;
      }
      const l = data.l;
      const r = data.r;
      if (!l || !r) return;
      const n = Math.min(l.length, r.length);
      this.pendingFrames = Math.max(0, this.pendingFrames - n);
      for (let i = 0; i < n; i++) {
        if (this.available >= this.cap) {
          this.readIdx = (this.readIdx + 1) % this.cap;
          this.available--;
        }
        this.ringL[this.writeIdx] = l[i];
        this.ringR[this.writeIdx] = r[i];
        this.writeIdx = (this.writeIdx + 1) % this.cap;
        this.available++;
      }
    };
  }

  process(_inputs, outputs) {
    const out = outputs[0];
    if (!out || out.length === 0) return true;
    const outL = out[0];
    const outR = out[1] || out[0];
    const n = outL.length;
    for (let i = 0; i < n; i++) {
      if (this.available > 0) {
        outL[i] = this.ringL[this.readIdx];
        outR[i] = this.ringR[this.readIdx];
        this.readIdx = (this.readIdx + 1) % this.cap;
        this.available--;
      } else {
        outL[i] = 0;
        outR[i] = 0;
      }
    }
    if (this.available + this.pendingFrames < 8192) {
      this.pendingFrames += 4096;
      this.port.postMessage({ need: 4096 });
    }
    return true;
  }
}

registerProcessor("pal-synth-worklet", PalSynthWorkletProcessor);
