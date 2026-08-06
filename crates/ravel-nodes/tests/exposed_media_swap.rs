// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! End-to-end coverage for swapping media through an exposed parameter
//! declaration (REQ-PROJ-006, REQ-RENDER-005).
//!
//! `crates/ravel-core/src/exposed/apply.rs` pins what the swap does to the
//! **document**: which asset id the media node names, and where that id
//! resolves to. It cannot pin the half that matters to a caller — that the
//! next evaluation decodes a different file — because decoding needs FFmpeg
//! and `ravel-core` has none.
//!
//! This test closes that gap, and it lives here rather than in `ravel-media`
//! because the two halves only meet in this crate: `ravel-media` supplies the
//! decoder but cannot see `MediaProcessor`, which is what reads `asset_id` out
//! of the document and asks for a frame.
//!
//! The fixtures are two one-second solid-colour clips, red and green, so the
//! assertion is a decoded pixel rather than a path comparison: a regression
//! that swaps the reference without moving what evaluation reads (or the
//! reverse) changes these numbers. They need the `ffmpeg` feature and the
//! `ffmpeg` CLI to synthesize the fixtures, so — like
//! `ravel-media/tests/integration_ffmpeg.rs` — they do not run under the
//! default `cargo test --workspace`.

#[cfg(feature = "ffmpeg")]
mod ffmpeg_tests {
    use ravel_core::composition::{AssetPath, Composition, Document, Layer};
    use ravel_core::eval::{EvalContext, Evaluator};
    use ravel_core::exposed::apply::{AssetContext, apply};
    use ravel_core::exposed::{ExposedBinding, ExposedParameter, ExposedParameters, ExposedValue};
    use ravel_core::graph::{Graph, Node, ParameterValue};
    use ravel_core::id::{CompId, DataTypeId, LayerId, NodeId};
    use ravel_core::types::{FrameBuffer, FrameRate};
    use ravel_nodes::media::MediaProcessor;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Arc;

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };

    fn media_node() -> NodeId {
        NodeId::new(1)
    }

    /// Write a one-second 32x32 clip of the solid colour `colour` into `dir`.
    fn generate(dir: &Path, name: &str, colour: &str) -> PathBuf {
        let path = dir.join(name);
        let output = Command::new("ffmpeg")
            .args([
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("color=c={colour}:s=32x32:d=1:r=30"),
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&path)
            .output()
            .expect("ffmpeg CLI not found");
        assert!(
            output.status.success(),
            "ffmpeg failed to generate {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        path
    }

    /// A document whose only layer holds a media node pointing at `original`,
    /// with `plate` declared against that node's asset reference.
    fn document(original: &Path) -> Document {
        let network = Graph::new()
            .add_node(
                Node::new(media_node(), "media")
                    .with_output("frame", DataTypeId::FRAME_BUFFER)
                    .with_param("asset_id", ParameterValue::String("original".into())),
            )
            .unwrap();
        let comp = Composition::new(CompId::new(1), "Main", (32, 32), FPS, 30)
            .add_layer(Layer::new(LayerId::new(1), "Plate", network).with_time(0, 0, 30));
        Document::default()
            .with_composition(comp)
            .with_media_asset("original", original)
            .with_exposed_parameters(
                ExposedParameters::from_declarations([ExposedParameter::inferred(
                    "plate",
                    ExposedValue::Media(AssetPath::Absolute(original.to_path_buf())),
                    ExposedBinding::new(media_node(), "asset_id"),
                )
                .unwrap()])
                .unwrap(),
            )
    }

    /// Decode frame 0 of the document's media node, the way a render would:
    /// the processor reads `asset_id` off the node and the location out of the
    /// document's asset table.
    fn decode_first_frame(document: &Document) -> FrameBuffer {
        let network = &document
            .compositions
            .values()
            .next()
            .expect("one composition")
            .layers
            .head()
            .expect("one layer")
            .network;

        let mut evaluator = Evaluator::new();
        evaluator.set_document(Arc::new(document.clone()));
        let node = network.node(media_node()).expect("the media node");
        evaluator.register(media_node(), Arc::new(MediaProcessor::from_node(node)));

        let value = evaluator
            .evaluate(network, media_node(), &EvalContext::new(0, FPS, (32, 32)))
            .expect("the media node decodes");
        value
            .downcast_ref::<FrameBuffer>()
            .expect("the media node produces a CPU frame")
            .clone()
    }

    fn centre_pixel(frame: &FrameBuffer) -> [f32; 4] {
        let x = frame.width / 2;
        let y = frame.height / 2;
        let idx = ((y * frame.width + x) * 4) as usize;
        frame.as_f32()[idx..idx + 4].try_into().unwrap()
    }

    /// The completion criterion in full: applying a media declaration changes
    /// what evaluation decodes. Red before, green after, with the same
    /// document, the same node and the same frame.
    #[test]
    fn applying_a_media_declaration_changes_the_decoded_frame() {
        let dir = tempfile::tempdir().expect("a temporary project root");
        let original = generate(dir.path(), "original.mp4", "red");
        generate(dir.path(), "replacement.mp4", "green");

        let before = centre_pixel(&decode_first_frame(&document(&original)));
        assert!(
            before[0] > 0.5 && before[1] < 0.3,
            "the fixture decodes as red before the swap: {before:?}"
        );

        let values: HashMap<String, ExposedValue> = [(
            "plate".to_string(),
            ExposedValue::Media(AssetPath::Relative("./replacement.mp4".into())),
        )]
        .into_iter()
        .collect();
        let applied = apply(
            document(&original),
            &values,
            AssetContext::rooted(dir.path()),
        )
        .expect("the replacement is there");
        assert!(applied.issues.is_empty(), "{:?}", applied.issues);

        let after = centre_pixel(&decode_first_frame(&applied.document));
        assert!(
            after[1] > 0.4 && after[0] < 0.3,
            "the same node decodes green after the swap: {after:?}"
        );
        assert_ne!(before, after);
        assert_eq!(
            (applied.document.compositions.values().next().unwrap()).resolution,
            (32, 32),
            "and the composition kept its own extent (the swap is a reference \
             substitution, not a re-fit)"
        );
    }

    /// The other half of the contract, exercised where it can actually be
    /// seen: a declaration pointing at a file that is not there is refused
    /// before evaluation, rather than decoding into a blank frame.
    #[test]
    fn an_absent_replacement_never_reaches_the_decoder() {
        let dir = tempfile::tempdir().expect("a temporary project root");
        let original = generate(dir.path(), "original.mp4", "red");

        let values: HashMap<String, ExposedValue> = [(
            "plate".to_string(),
            ExposedValue::Media(AssetPath::Relative("./gone.mp4".into())),
        )]
        .into_iter()
        .collect();
        assert!(
            apply(
                document(&original),
                &values,
                AssetContext::rooted(dir.path())
            )
            .is_err(),
            "an absent file is refused, not decoded"
        );
    }
}
