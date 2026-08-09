# Vellum

Vellum is a small image and PDF viewer written in pure Rust. The same
workspace targets host systems through ScarletUI/Winit and Scarlet OS through
ScarletUI/SWS.

The viewer is split into two crates:

- `vellum-core` loads common image formats and PDFs, then rasterizes them into
  RGBA bitmaps. It has no Scarlet dependency.
- `vellum` provides the desktop and ScarletUI frontends, including page
  navigation, zooming, and scrolling.

PDF pages are rendered lazily and cached. PDF rasterization uses Hayro; image
decoding uses the pure-Rust `image` codecs enabled in `vellum-core`.

## Host

On an Apple Silicon Mac:

```bash
cargo run -p vellum --target aarch64-apple-darwin -- path/to/file.pdf
```

Use `x86_64-apple-darwin` on an Intel Mac, or the appropriate host target on
Linux. The headless core is also useful for smoke tests:

```bash
cargo run -p vellum --no-default-features -- \
  --dump-png page.png --page 0 path/to/file.pdf
```

## Scarlet OS

Enter the SDK development shell and build for either supported Scarlet target:

```bash
nix develop
cargo build -p vellum --release --target riscv64gc-unknown-scarlet
cargo build -p vellum --release --target aarch64-unknown-scarlet
```

The resulting `vellum` binary can be placed in a Scarlet filesystem image as
`/bin/vellum`. Its application identifier is `org.scarlet-os.vellum`.

## Controls

- `←` / `PageUp`: previous page
- `→` / `PageDown` / `Space`: next page
- `Home` / `End`: first or last page
- `Fit`: fit the whole page inside the window
- `100%`: show the page at its original size
- `+` / `-`: zoom in or out
- `0`: reset zoom

## License

Vellum is dual-licensed under the MIT License or Apache License 2.0.
