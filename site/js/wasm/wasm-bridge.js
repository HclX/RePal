/**
 * Loader for `./wasm/pal.wasm` plus the audio-synth helpers used by `AudioManager`.
 * Everything else talks to the module through its raw `exports` (`core_*`, `wasm_ui_*`, ...).
 */

export class PalWasmBridge {
  constructor() {
    this.instance = null;
    this.exports = null;
  }

  async init(wasmUrl = "./wasm/pal.wasm") {
    const resp = await fetch(wasmUrl);
    if (!resp.ok) {
      throw new Error(`Failed to fetch WASM (${resp.status}): ${wasmUrl}`);
    }
    const { instance } = await WebAssembly.instantiate(await resp.arrayBuffer(), {});
    this.instance = instance;
    this.exports = instance.exports;
    return this;
  }

  /** Always re-read: the buffer is replaced whenever linear memory grows. */
  get memoryBuffer() {
    return this.exports.memory.buffer;
  }

  synthInit(sampleRate) {
    this.exports.wasm_synth_init(sampleRate >>> 0);
  }

  synthLoadMidi(midiUint8, loop = true) {
    const len = midiUint8.byteLength;
    const ptr = this.exports.wasm_scratch_ptr(len);
    new Uint8Array(this.memoryBuffer, ptr, len).set(midiUint8);
    return this.exports.wasm_synth_load_midi(len, loop ? 1 : 0) === 1;
  }

  synthStop() {
    this.exports.wasm_synth_stop();
  }

  synthSetHdMode(hd) {
    this.exports.wasm_synth_set_hd_mode(hd ? 1 : 0);
  }

  synthSetVolume(vol) {
    this.exports.wasm_synth_set_volume(Number(vol));
  }

  synthRenderInto(outLeftF32, outRightF32) {
    const total = Math.min(outLeftF32.length, outRightF32.length);
    let offset = 0;
    while (offset < total) {
      const req = Math.min(4096, total - offset);
      const rendered = this.exports.wasm_synth_render(req) || req;
      const n = Math.min(rendered, total - offset);
      outLeftF32.set(new Float32Array(this.memoryBuffer, this.exports.wasm_audio_left_ptr(), n), offset);
      outRightF32.set(new Float32Array(this.memoryBuffer, this.exports.wasm_audio_right_ptr(), n), offset);
      offset += n;
    }
  }
}
