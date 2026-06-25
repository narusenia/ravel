# TASK-009: FFmpeg Decode/Encode Integration

## Spec
See `docs/implementation/tasks/TASK-009.md` and `docs/specifications/` for full details.

## Summary
Integrate FFmpeg into Ravel's media pipeline via dynamic linking (LGPL compliance). Use `ffmpeg-next` crate for video/audio/image decode and encode.

## Steps
1. Set up FFmpeg dynamic linking (LGPL compliant)
2. Implement video decoder (`ffmpeg-next`)
3. Implement audio decoder
4. Implement encoder pipeline
5. Define `MediaReader` / `MediaWriter` traits
6. Implement format auto-detection
7. Support H.264, H.265, AV1, ProRes, DNxHR decode
8. Support MP4, MOV, MKV, WebM containers
9. Support image sequences (EXR, PNG, TIFF, DPX)
10. Integration tests with sample media files

## Target crates
- `crates/ravel-media/`
- `crates/ravel-core/` (trait definitions)

## Completion criteria
- FFmpeg dynamically linked, LGPL compliant
- `MediaReader` decodes H.264, H.265, AV1, ProRes, DNxHR
- `MediaWriter` encodes via pipeline
- MP4, MOV, MKV, WebM container read/write
- EXR, PNG, TIFF, DPX image sequence read
- Format auto-detection works
- Integration tests pass with sample media
