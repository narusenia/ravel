# TASK-011: Audio Engine (CPAL + DSP)

## Spec
See `docs/implementation/tasks/TASK-011.md` and `docs/specifications/` for full details.

## Summary
Build audio output foundation with CPAL and a multi-track mixer using dasp. Real-time processing on a dedicated high-priority audio thread, with sample rate conversion (rubato), basic effects, video sync, and waveform data for UI.

## Steps
1. Set up CPAL audio output
2. Dedicated high-priority real-time audio thread
3. Multi-track mixer with dasp
4. Sample rate conversion via rubato
5. Basic effects (gain, fade)
6. Video-audio sync mechanism
7. Waveform data generation for UI display

## Target crates
- `crates/ravel-audio/` (new crate)

## Completion criteria
- CPAL audio output works correctly
- Real-time audio processing on dedicated high-priority thread
- Multi-track mixing works correctly
- Automatic sample rate conversion for different source rates
- Gain and fade effects applicable
- Video-audio sync mechanism functional
- Waveform data generated for UI display
