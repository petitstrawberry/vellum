# Vellum

Vellum is a small image and PDF viewer written in pure Rust, built with
ScarletUI.

The viewer is split into two crates:

- `vellum-core` loads common image formats and PDFs, then rasterizes them into
  RGBA bitmaps. It has no Scarlet dependency.
- `vellum` provides the frontends, including page navigation, zooming, and
  scrolling.

PDF pages are rendered lazily and cached. PDF rasterization uses Hayro; image
decoding uses the pure-Rust `image` codecs enabled in `vellum-core`.

## Run

```bash
cargo run -p vellum -- path/to/file.pdf
```

The headless core is also useful for smoke tests:

```bash
cargo run -p vellum --no-default-features -- \
  --dump-png page.png --page 0 path/to/file.pdf
```

## Controls

- `←` / `PageUp`: previous page
- `→` / `PageDown` / `Space`: next page
- `Home` / `End`: first or last page
- `Fit`: fit the whole page inside the window
- `100%`: show the page at its original size
- `+` / `-`: zoom in or out
- `0`: reset zoom

## License

Vellum is licensed under the MIT License.
