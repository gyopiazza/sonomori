# Sonomori

Sonomori is an instrument plugin that slices an imported sound and plays it as a tempo-synced loop. Load a file, and it finds transients, then builds a one- or two-bar pattern across four lanes (Low, Mid, High, Extra) in sixteenth notes. The same seed and macro settings always rebuild the same pattern.

It follows the host tempo and transport. The standalone app uses the Audition BPM in the editor instead. The imported file path is stored with the DAW project and reloaded on the next session. Files longer than 30 seconds are trimmed.

Supported import formats include WAV, FLAC, MP3, AAC, ALAC, and Ogg Vorbis. Output is stereo or mono.

## Build

You need Rust 1.88 or newer, plus the [nice-plug](https://codeberg.org/RustAudio/nice-plug) bundler:

```sh
cargo install cargo-nice-plug
cargo nice-plug bundle sonomori --release
```

Bundles are written to `target/bundled/`:

- `Sonomori.clap`
- `Sonomori.vst3`
- `Sonomori.app` on macOS (the standalone app)

Run the standalone without bundling:

```sh
cargo run --release
```

Copy the plugin bundles into your host's plugin folder. On macOS that is `~/Library/Audio/Plug-Ins/CLAP` and `~/Library/Audio/Plug-Ins/VST3`.

## Licenses

Sonomori is MIT. Third-party attribution, including the VST 3 interface, the MPL libraries, and the embedded fonts, is in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Ship that file with the plugin bundles and the standalone app.

VST is a registered trademark of Steinberg Media Technologies GmbH.
