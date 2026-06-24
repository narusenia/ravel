// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Robustness ("fuzz") tests for the `.ravprj` parsing surface.
//!
//! The acceptance criteria require that the project parsers survive 100 000
//! malformed inputs without crashing. A full `cargo-fuzz` harness needs a
//! nightly toolchain and a separate target crate; this integration test gives
//! equivalent coverage inside the normal `cargo test` run by feeding a large,
//! deterministic stream of adversarial inputs at every parser entry point and
//! asserting each one returns a `Result` (i.e. never panics).
//!
//! Determinism: a small xorshift PRNG is seeded with a fixed constant so a
//! failure is always reproducible. No external dependency is pulled in.

use ravel_app::project::asset::AssetCollection;
use ravel_app::project::container::RawArchive;
use ravel_app::project::graph_doc::GraphDoc;
use ravel_app::project::settings::SettingsLayer;
use ravel_app::project::ProjectFile;

/// Minimal deterministic xorshift64* PRNG.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn byte(&mut self) -> u8 {
        (self.next_u64() & 0xff) as u8
    }

    /// A random length in `0..=max`.
    fn len(&mut self, max: usize) -> usize {
        (self.next_u64() as usize) % (max + 1)
    }
}

/// Build a buffer of random bytes.
fn random_bytes(rng: &mut Rng, max_len: usize) -> Vec<u8> {
    let n = rng.len(max_len);
    (0..n).map(|_| rng.byte()).collect()
}

/// Build a buffer biased toward valid zip / structured tokens, so the fuzzer
/// probes deeper code paths than pure noise usually reaches.
fn structured_bytes(rng: &mut Rng) -> Vec<u8> {
    // Common signatures / fragments worth corrupting.
    const SEEDS: &[&[u8]] = &[
        b"PK\x03\x04",                         // zip local file header
        b"PK\x05\x06",                         // zip end-of-central-directory
        b"GraphDoc(nodes:[],edges:[])",        // valid RON
        b"GraphDoc(nodes:[Node(",              // truncated RON
        b"{\"assets\":[]}",                    // valid assets JSON
        b"{\"assets\":[{\"id\":",              // truncated JSON
        b"[color]\nworking_space=\"ACEScg\"",  // valid TOML
        b"[playback\nframe_rate=",             // broken TOML
        b"{\"format_version\":1,\"color_space\":\"x\"}", // v1 manifest fragment
    ];
    let seed = SEEDS[(rng.next_u64() as usize) % SEEDS.len()];
    let mut buf = seed.to_vec();
    // Append/flip random bytes to corrupt it.
    let extra = rng.len(48);
    for _ in 0..extra {
        buf.push(rng.byte());
    }
    if !buf.is_empty() {
        let idx = (rng.next_u64() as usize) % buf.len();
        buf[idx] = rng.byte();
    }
    buf
}

#[test]
fn fuzz_parsers_survive_100k_inputs() {
    const ITERATIONS: usize = 100_000;
    // Fixed seed → reproducible failures.
    let mut rng = Rng::new(0x5256_454C_5052_4A21);

    for i in 0..ITERATIONS {
        // Alternate between pure noise and structured-but-corrupt inputs.
        let bytes = if i % 2 == 0 {
            random_bytes(&mut rng, 256)
        } else {
            structured_bytes(&mut rng)
        };

        // 1) Zip container parser.
        let archive_result = RawArchive::from_bytes(&bytes);

        // 2) If by chance it parsed as a zip, push it through the full project
        //    decoder too.
        if let Ok(archive) = &archive_result {
            let _ = ProjectFile::from_archive(archive);
        }

        // 3) Text parsers operate on lossy-UTF8 views of the same bytes.
        let text = String::from_utf8_lossy(&bytes);
        let _ = GraphDoc::from_ron(&text);
        let _ = AssetCollection::from_json(&text);
        let _ = SettingsLayer::from_toml(&text);
    }

    // Reaching here means no parser panicked across 100k adversarial inputs.
}
