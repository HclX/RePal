# Unified PalRemake Build & Test Orchestration (Issue #45)
CARGO ?= cargo
NODE ?= node
PYTHON ?= python3

# Desktop audio output (cpal) is opt-in on Linux via the `audio` feature (built-in on macOS /
# Windows). Enable it automatically when ALSA headers are available, matching release builds.
ifeq ($(shell uname -s),Linux)
  ifeq ($(shell pkg-config --exists alsa 2>/dev/null && echo yes),yes)
    REPAL_FEATURES ?= --features audio
  else
    REPAL_FEATURES ?=
    REPAL_AUDIO_HINT := 1
  endif
endif

.PHONY: help build-wasm build-repal build-desktop run-repal run-desktop test-rust test-rust-lowmem test-js test-py test fuzz fuzz-nightly

help:
	@echo "Targets:"
	@echo "  make build-wasm       Build pal-wasm release & copy to site/wasm/pal.wasm"
	@echo "  make build-repal      Build unified native binary (target/release/repal)"
	@echo "  make build-desktop    Alias for build-repal"
	@echo "  make run-repal        Run native repal GUI locally against site/assets"
	@echo "  make run-desktop      Alias for run-repal"
	@echo "  make test-rust        Run full Cargo workspace tests"
	@echo "  make test-rust-lowmem Run Cargo workspace tests serially (-j 1, RUST_TEST_THREADS=1) for <=4 GB hosts"
	@echo "  make test-js          Run the headless browser-host suites against pal.wasm"
	@echo "  make test-py          Run Python static/data/hygiene pytest suite"
	@echo "  make fuzz             Run fast PR release fuzzing suite (core + A1/A2/A4 + text-mode + parsers)"
	@echo "  make fuzz-nightly     Run extended nightly fuzzing suite (3000 ticks x 4 seeds + deep parser fuzz)"
	@echo "  make test             Run all Rust + Node + Python verification suites"

build-wasm:
	$(CARGO) build -p pal-wasm --target wasm32-unknown-unknown --release
	mkdir -p site/wasm
	cp target/wasm32-unknown-unknown/release/pal_wasm.wasm site/wasm/pal.wasm

build-repal build-desktop:
ifdef REPAL_AUDIO_HINT
	@echo "warning: ALSA headers not found; building repal WITHOUT audio (install libasound2-dev pkg-config)"
endif
	$(CARGO) build -p pal-desktop --bin repal --release $(REPAL_FEATURES)

run-repal run-desktop:
	$(CARGO) run -p pal-desktop --bin repal --release $(REPAL_FEATURES) -- --assets site/assets

test-rust:
	$(CARGO) test --workspace

test-rust-lowmem:
	RUST_TEST_THREADS=1 $(CARGO) test --workspace -j 1

test-js: build-wasm
	$(NODE) tools/tests/test_headless_harness.mjs
	$(NODE) tools/tests/test_host.mjs
	$(NODE) tools/test_rust_core_host.mjs

test-py:
	$(PYTHON) -m pytest tools/tests/ -v

fuzz:
	$(CARGO) test --offline --release -p pal-core --test fuzz_invariants --test fuzz_parsers -- --nocapture
	$(CARGO) test --offline --release -p pal-runtime --features disk --test fuzz_runtime -- --nocapture
	$(CARGO) test --offline --release -p pal-headless --test fuzz_text_mode -- --nocapture

fuzz-nightly:
	PAL_FUZZ_TICKS=3000 PAL_FUZZ_SEEDS=4 PAL_CONT_WARMUP=400 PAL_CONT_FOLLOW=600 PAL_PARSER_FUZZ_ITERS=3000 $(CARGO) test --offline --release -p pal-core --test fuzz_invariants --test fuzz_parsers -- --nocapture
	PAL_FUZZ_TICKS=600 PAL_PARSER_FUZZ_ITERS=3000 $(CARGO) test --offline --release -p pal-runtime --features disk --test fuzz_runtime -- --nocapture
	PAL_FUZZ_TICKS=1500 PAL_PARSER_FUZZ_ITERS=3000 $(CARGO) test --offline --release -p pal-headless --test fuzz_text_mode -- --nocapture

test: test-rust test-js test-py
