//! Deterministic game RNG.
//!
//! Every random decision in the game core (script branches, battle rolls, monster wandering) must
//! draw from one seeded [`GameRng`] whose state is part of the save/replay snapshot; nothing may
//! call a platform RNG. The generator is Mulberry32, bit-identical to the legacy
//! `FlightRecorder.nextRandom()` in `site/js/debug/flight-recorder.js`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameRng {
    pub state: u32,
}

impl Default for GameRng {
    fn default() -> Self {
        Self::new(0x0095_1995)
    }
}

impl GameRng {
    pub fn new(seed: u32) -> Self {
        Self { state: seed }
    }

    pub fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_add(0x6d2b_79f5);
        let mut t = self.state;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        t ^ (t >> 14)
    }

    /// Uniform float in `[0, 1)`.
    pub fn next_f64(&mut self) -> f64 {
        self.next_u32() as f64 / 4_294_967_296.0
    }

    /// Uniform integer in `[0, n)` (returns 0 for `n == 0`).
    pub fn below(&mut self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            (self.next_f64() * n as f64) as u32
        }
    }

    /// Inclusive uniform integer in `[from, to]` matching SDLPAL `RandomLong(from, to)`.
    pub fn random_long(&mut self, from: i32, to: i32) -> i32 {
        if to <= from {
            from
        } else {
            let span = (to as i64 - from as i64 + 1) as u32;
            from + self.below(span) as i32
        }
    }

    /// Uniform float in `[from, to)` matching SDLPAL `RandomFloat(from, to)`.
    pub fn random_float(&mut self, from: f64, to: f64) -> f64 {
        if to <= from {
            from
        } else {
            from + self.next_f64() * (to - from)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_js_mulberry32_reference() {
        // Reference values from the JS implementation with seed 0x951995:
        // node -e "let s=0x951995;const n=()=>{let t=(s+=0x6d2b79f5);t=Math.imul(t^(t>>>15),t|1);
        //   t^=t+Math.imul(t^(t>>>7),t|61);return((t^(t>>>14))>>>0)};console.log(n(),n(),n())"
        let mut r = GameRng::default();
        let got = [r.next_u32(), r.next_u32(), r.next_u32()];
        assert_eq!(got, JS_REFERENCE);
    }

    const JS_REFERENCE: [u32; 3] = [4_274_146_162, 3_549_527_442, 336_042_075];
}
