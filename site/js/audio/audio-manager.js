/**
 * Dual-Mode WebAudio Engine:
 * - Streams real-time stereo audio from `pal-wasm` DualSynth:
 *     HD Mode (`true`): Ancient Chinese Guzheng / Dizi / Erhu acoustic synthesis + Stereo Hall Reverb
 *     1995 Mode (`false`): Authentic 2-Operator OPL2 FM SoundBlaster synthesis
 *   Switching `[F2]` morphs timbre instantaneously mid-note!
 * - Plays remastered 22.05 kHz 16-bit WAV sound effects from `./assets/audio/sfx/`.
 */

import { resolveAssetBaseUrl, withAssetVersion, withCodeVersion } from "../host/asset-provider.js";

export class AudioManager {
  constructor(wasmBridge) {
    this.wasm = wasmBridge;
    this.assetBaseUrl = resolveAssetBaseUrl();
    this.packManager = null;
    this.ctx = null;
    this.scriptNode = null;
    this.workletNode = null;
    this.masterGain = null;
    this.sfxGain = null;
    this.hdMode = true;
    this.musicEnabled = true;
    this.sfxEnabled = true;
    this.muted = false;
    this.currentTrackId = 0;
    this.currentLoop = true;
    this.pendingTrack = null;
    this.midiCache = new Map();
    this.hdBgmCache = new Map();
    this.hdBgmMissing = new Set();
    this.hdBgmSource = null;
    this.hdBgmActive = false;
    this.sfxCache = new Map();
    this.sfxInFlight = new Map();
    this.unlocked = false;
    this._unlockInstalled = false;
  }

  installGestureUnlock() {
    if (this._unlockInstalled || typeof window === "undefined") return;
    this._unlockInstalled = true;
    const onGesture = async () => {
      const ok = await this.unlockOnGesture();
      if (ok && this.ctx?.state === "running") {
        window.removeEventListener("pointerdown", onGesture, true);
        window.removeEventListener("keydown", onGesture, true);
        window.removeEventListener("touchstart", onGesture, true);
        window.removeEventListener("click", onGesture, true);
      }
    };
    window.addEventListener("pointerdown", onGesture, true);
    window.addEventListener("keydown", onGesture, true);
    window.addEventListener("touchstart", onGesture, true);
    window.addEventListener("click", onGesture, true);
  }

  ensureContext() {
    if (this.ctx) {
      if (this.ctx.state === "running") {
        this.unlocked = true;
      } else if (this.ctx.state === "suspended") {
        this.ctx
          .resume()
          .then(() => {
            if (this.ctx?.state === "running") {
              this.unlocked = true;
              this._flushPendingTrack();
            }
          })
          .catch(() => {});
      }
      return true;
    }
    const AudioCtx =
      typeof window !== "undefined" ? window.AudioContext || window.webkitAudioContext : null;
    if (!AudioCtx) return false;

    try {
      // Use native device sample rate (no hardcoded 44100Hz that throws on 48kHz-only devices, #33)
      this.ctx = new AudioCtx();
    } catch (err) {
      console.warn("[AudioManager] AudioContext creation failed:", err);
      return false;
    }

    this.wasm.synthInit(this.ctx.sampleRate);
    this.wasm.synthSetHdMode(this.hdMode);
    this.wasm.synthSetVolume(this.musicEnabled ? 0.72 : 0.0);

    this.masterGain = this.ctx.createGain();
    this.masterGain.gain.value = this.musicEnabled ? 0.9 : 0.0;
    this.masterGain.connect(this.ctx.destination);

    this.sfxGain = this.ctx.createGain();
    this.sfxGain.gain.value = this.sfxEnabled ? 0.85 : 0.0;
    this.sfxGain.connect(this.ctx.destination);

    this._attachSynthOutputNode();
    this.unlocked = this.ctx.state === "running";
    if (this.ctx.state === "suspended") {
      this.ctx
        .resume()
        .then(() => {
          if (this.ctx?.state === "running") {
            this.unlocked = true;
            this._flushPendingTrack();
          }
        })
        .catch(() => {});
    } else {
      this._flushPendingTrack();
    }
    return true;
  }

  _flushPendingTrack() {
    if (this.pendingTrack && this.ctx) {
      const { trackId, loop } = this.pendingTrack;
      this.pendingTrack = null;
      this.playMusic(trackId, loop, true);
    }
  }

  _attachSynthOutputNode() {
    // Prefer AudioWorkletNode in secure contexts; fall back cleanly to ScriptProcessorNode (#33)
    if (this.ctx?.audioWorklet && typeof AudioWorkletNode !== "undefined") {
      const workletUrl = withCodeVersion("./js/audio/pal-synth-worklet.js");
      this.ctx.audioWorklet
        .addModule(workletUrl)
        .then(() => {
          if (!this.ctx || !this.masterGain) return;
          const node = new AudioWorkletNode(this.ctx, "pal-synth-worklet", {
            numberOfInputs: 0,
            numberOfOutputs: 1,
            outputChannelCount: [2],
          });
          node.port.onmessage = (ev) => {
            const need = ev.data?.need || 2048;
            const outL = new Float32Array(need);
            const outR = new Float32Array(need);
            if (this.musicEnabled && !this.hdBgmActive && this.wasm?.exports) {
              this.wasm.synthRenderInto(outL, outR);
            }
            node.port.postMessage({ l: outL, r: outR }, [outL.buffer, outR.buffer]);
          };
          node.connect(this.masterGain);
          this.workletNode = node;
          // Kick-start initial buffer fill (4096 frames ~85ms x2 for smooth TV/Xbox playback)
          for (let k = 0; k < 2; k++) {
            const initL = new Float32Array(4096);
            const initR = new Float32Array(4096);
            if (this.musicEnabled && this.wasm?.exports) {
              this.wasm.synthRenderInto(initL, initR);
            }
            node.port.postMessage({ l: initL, r: initR }, [initL.buffer, initR.buffer]);
          }
        })
        .catch(() => {
          this._attachScriptProcessorFallback();
        });
      return;
    }
    this._attachScriptProcessorFallback();
  }

  _attachScriptProcessorFallback() {
    if (!this.ctx || !this.masterGain || this.scriptNode || this.workletNode) return;
    if (typeof this.ctx.createScriptProcessor !== "function") return;
    // 8192 samples (~170ms at 48kHz) prevents underruns on Xbox Edge / TV browsers over plain HTTP LAN.
    const bufSize = 8192;
    this.scriptNode = this.ctx.createScriptProcessor(bufSize, 0, 2);
    this.scriptNode.onaudioprocess = (ev) => {
      const outL = ev.outputBuffer.getChannelData(0);
      const outR = ev.outputBuffer.getChannelData(1);
      if (!this.musicEnabled || this.hdBgmActive) {
        outL.fill(0);
        outR.fill(0);
        return;
      }
      this.wasm.synthRenderInto(outL, outR);
    };
    this.scriptNode.connect(this.masterGain);
  }

  async unlockOnGesture() {
    if (!this.ensureContext() || !this.ctx) return false;
    if (this.ctx.state === "suspended") {
      try {
        await this.ctx.resume();
      } catch {
        return false;
      }
    }
    if (this.ctx.state === "running") {
      this.unlocked = true;
      if (this.pendingTrack) {
        this._flushPendingTrack();
      } else if (this.currentTrackId > 0 && !this.hdBgmActive && this.hdMode) {
        this.playMusic(this.currentTrackId, this.currentLoop, true);
      }
      return true;
    }
    return false;
  }

  _clearWorklet() {
    this.workletNode?.port?.postMessage({ type: "clear" });
  }

  _stopHdBgm() {
    if (this.hdBgmSource) {
      try {
        this.hdBgmSource.stop();
        this.hdBgmSource.disconnect();
      } catch {
        // Ignore already-stopped source
      }
      this.hdBgmSource = null;
    }
    this.hdBgmActive = false;
  }

  async _loadHdBgmBuffer(id) {
    if (this.hdBgmMissing.has(id)) return null;
    if (this.hdBgmCache.has(id)) {
      const buf = this.hdBgmCache.get(id);
      this.hdBgmCache.delete(id);
      this.hdBgmCache.set(id, buf);
      return buf;
    }
    const pad = String(id).padStart(3, "0");
    for (const ext of ["mp3", "ogg"]) {
      const relKey = `audio/bgm_hd/mus_${pad}.${ext}`;
      try {
        let raw = null;
        if (this.packManager) {
          const u8 = await this.packManager.readBytes(relKey);
          if (u8) raw = u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength);
        }
        if (!raw && !this.packManager) {
          const url = withAssetVersion(`${this.assetBaseUrl}/${relKey}`);
          const resp = await fetch(url);
          if (resp.ok) raw = await resp.arrayBuffer();
        }
        if (!raw) continue;
        const decoded = await this.ctx.decodeAudioData(raw);
        if (this.hdBgmCache.size >= 6) {
          const oldest = this.hdBgmCache.keys().next().value;
          if (oldest !== undefined) this.hdBgmCache.delete(oldest);
        }
        this.hdBgmCache.set(id, decoded);
        return decoded;
      } catch {
        // Try next format or fall back to DualSynth MIDI
      }
    }
    // Only cache permanent missing for loose-file mode; in pack mode the track's chapter `.rpk`
    // may be mounted moments later by `ensureScenePacks`.
    if (!this.packManager) {
      this.hdBgmMissing.add(id);
    }
    return null;
  }

  setHdMode(hd) {
    const next = Boolean(hd);
    const changed = next !== this.hdMode;
    this.hdMode = next;
    if (this.wasm?.exports) {
      this.wasm.synthSetHdMode(this.hdMode);
    }
    this._clearWorklet();
    if (changed && this.currentTrackId > 0 && this.ctx) {
      const tid = this.currentTrackId;
      const lp = this.currentLoop;
      this.playMusic(tid, lp, true);
    }
  }

  setMusicEnabled(enabled) {
    this.musicEnabled = Boolean(enabled);
    this.muted = !this.musicEnabled && !this.sfxEnabled;
    if (this.wasm?.exports) {
      this.wasm.synthSetVolume(this.musicEnabled ? 0.72 : 0.0);
    }
    if (this.masterGain) {
      this.masterGain.gain.value = this.musicEnabled ? 0.9 : 0.0;
    }
    return this.musicEnabled;
  }

  toggleMusic() {
    return this.setMusicEnabled(!this.musicEnabled);
  }

  setSfxEnabled(enabled) {
    this.sfxEnabled = Boolean(enabled);
    this.muted = !this.musicEnabled && !this.sfxEnabled;
    if (this.sfxGain) {
      this.sfxGain.gain.value = this.sfxEnabled ? 0.85 : 0.0;
    }
    return this.sfxEnabled;
  }

  toggleSfx() {
    return this.setSfxEnabled(!this.sfxEnabled);
  }

  toggleMute() {
    const shouldMute = this.musicEnabled || this.sfxEnabled;
    this.setMusicEnabled(!shouldMute);
    this.setSfxEnabled(!shouldMute);
    return this.muted;
  }

  async playMusic(trackId, loop = true, force = false) {
    const id = Number(trackId) | 0;
    if (id <= 0) {
      this.stopMusic();
      return;
    }
    if (!force && id === this.currentTrackId && loop === this.currentLoop && this.ctx) {
      return;
    }
    if (!this.ctx || this.ctx.state === "suspended") {
      this.pendingTrack = { trackId: id, loop };
      if (this.ctx) {
        this.ctx.resume().then(() => {
          if (this.ctx?.state === "running") {
            this.unlocked = true;
            this._flushPendingTrack();
          }
        }).catch(() => {});
      }
      return;
    }
    this.currentTrackId = id;
    this.currentLoop = loop;
    if (this.ctx.state === "suspended") {
      this.ctx.resume().catch(() => {});
    }

    // Hybrid HD BGM (#3): when in HD mode, prefer pre-rendered `audio/bgm_hd/mus_XXX.{mp3,ogg}`
    // (0% WASM synth CPU), falling back seamlessly to DualSynth MIDI if not present.
    if (this.hdMode) {
      const hdBuf = await this._loadHdBgmBuffer(id);
      if (this.currentTrackId !== id) return;
      if (hdBuf) {
        this._stopHdBgm();
        this._clearWorklet();
        if (this.wasm?.exports) this.wasm.synthStop();
        const src = this.ctx.createBufferSource();
        src.buffer = hdBuf;
        src.loop = Boolean(loop);
        src.connect(this.masterGain);
        src.start(0);
        this.hdBgmSource = src;
        this.hdBgmActive = true;
        return;
      }
    }

    this._stopHdBgm();
    try {
      let bytes = this.midiCache.get(id);
      if (!bytes) {
        const relKey = `audio/midi/mus_${String(id).padStart(3, "0")}.mid`;
        if (this.packManager) {
          bytes = await this.packManager.readBytes(relKey);
        }
        if (!bytes) {
          const url = withAssetVersion(`${this.assetBaseUrl}/${relKey}`);
          const resp = await fetch(url);
          if (!resp.ok) {
            if (this.currentTrackId === id) this.currentTrackId = 0;
            return;
          }
          bytes = new Uint8Array(await resp.arrayBuffer());
        }
        this.midiCache.set(id, bytes);
      }
      if (this.currentTrackId === id) {
        this._clearWorklet();
        this.wasm.synthLoadMidi(bytes, loop);
      }
    } catch (err) {
      if (this.currentTrackId === id) this.currentTrackId = 0;
      console.warn("[AudioManager] MIDI load skipped:", id, err);
    }
  }

  stopMusic() {
    this.currentTrackId = 0;
    this.pendingTrack = null;
    this._stopHdBgm();
    this._clearWorklet();
    if (this.wasm?.exports) {
      this.wasm.synthStop();
    }
  }

  async playSfx(sfxId) {
    const id = Number(sfxId) | 0;
    if (id <= 0 || !this.sfxEnabled) return;
    if (!this.ensureContext()) return;

    try {
      let audioBuf = this.sfxCache.get(id);
      if (!audioBuf) {
        let inflight = this.sfxInFlight.get(id);
        if (!inflight) {
          const relKey = `audio/sfx/sfx_${String(id).padStart(3, "0")}.wav`;
          const url = withAssetVersion(`${this.assetBaseUrl}/${relKey}`);
          inflight = (async () => {
            try {
              let raw = null;
              if (this.packManager) {
                const u8 = await this.packManager.readBytes(relKey);
                if (u8) raw = u8.buffer.slice(u8.byteOffset, u8.byteOffset + u8.byteLength);
              }
              if (!raw) {
                const resp = await fetch(url);
                if (!resp.ok) return null;
                raw = await resp.arrayBuffer();
              }
              const decoded = await this.ctx.decodeAudioData(raw);
              this.sfxCache.set(id, decoded);
              return decoded;
            } finally {
              this.sfxInFlight.delete(id);
            }
          })();
          this.sfxInFlight.set(id, inflight);
        }
        audioBuf = await inflight;
        if (!audioBuf) return;
      }
      const src = this.ctx.createBufferSource();
      src.buffer = audioBuf;
      if (!this.hdMode) {
        // Subtle retro playback character in 1995 mode
        const filter = this.ctx.createBiquadFilter();
        filter.type = "lowpass";
        filter.frequency.value = 5500;
        src.connect(filter);
        filter.connect(this.sfxGain);
      } else {
        src.connect(this.sfxGain);
      }
      src.start(0);
    } catch {
      // Ignore missing/empty VOC chunk indices gracefully
    }
  }
}
