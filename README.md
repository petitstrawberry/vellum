# Vellum

Vellum is a small image and PDF viewer written in pure Rust, built with
ScarletUI.

The viewer is split into two crates:

- `vellum-core` loads common image formats and PDFs, then rasterizes them into
  RGBA bitmaps and provides bounded image editing without a window system.
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

## Image editing

Raster images support clockwise rotation, cropping, resizing, arrows and text.
Choose **Crop**, drag inside the image and choose **Apply Crop**. Choose **Arrow**
and drag from the tail to the tip. **Text** opens the annotation settings; choose
**Place Text**, then click the image. Colors and arrow stroke widths are in the
toolbar. **View** or Escape leaves the editing tool. Resize keeps the aspect ratio
by default, using the requested width; turn that option off for independent axes.

Edits stay in memory until **Save As PNG**. The original image format is never
silently overwritten with PNG data. After the first export, Ctrl+S updates that
PNG; Ctrl+Shift+S chooses another destination. Open (Ctrl+O) and Close (Ctrl+W)
offer Save, Discard or Cancel for unsaved edits. Undo/Redo (Ctrl+Z,
Ctrl+Shift+Z or Ctrl+Y) includes every image operation, including text and arrows.
Text and arrows are rasterized when placed and can be removed with Undo.
PDF page navigation, zoom and fullscreen remain available; PDFs are read-only.
Native file pickers are available on Scarlet and macOS; unsupported host backends
report that limitation and keep the current image and its unsaved changes.

Editing is limited to 16 megapixels. Undo retains at most 32 operations and
128 MiB of image snapshots; older history is discarded when either limit is
reached. Input files are limited to 128 MiB, decoded images to 64 megapixels, PDF
page rasters to 4096 pixels per axis and the PDF cache to 128 MiB. Zoomed canvas
buffers have a pixel budget, so very large images may not reach 100% zoom.
PNG export stages replacements beside an existing destination; failed encoding
or writing preserves that destination. A new filename is reserved exclusively
before encoding, so a file created by another writer cannot be overwritten at
publication time. New output can be visible while encoding. Failed outputs are
removed only when their ownership can still be verified; failures report any
remaining path. Directory-entry persistence after abrupt power loss is not
guaranteed by the native platform.

## Native build requirements

Scarlet builds require Scarlet Rust commit `26d00a8` or newer, which supplies the
correct standard-library metadata, canonicalization, error mapping and file sync
behavior. Validation used the published `scarlet-rustc` output from
`scarlet-rust-nix` revision `79f356924d1a93b55904c6b2733d0b3da5ce0de5`.
The native staging adapter uses the current Scarlet checked VFS ABI for exclusive
creation and initial permissions because `std::fs::OpenOptions::create_new` still
uses a probe before creation and native chmod is unavailable. Native permissions
preserve owner read/write/execute capabilities; POSIX group bits, ownership,
ACLs and extended attributes are not exposed by that ABI.

```bash
cargo test --workspace
cargo check -p vellum --target aarch64-unknown-scarlet
```
