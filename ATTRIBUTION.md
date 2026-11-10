# Attribution: assets that are not code

Every bundled asset that is not program code — fonts, icons, sounds,
images, wallpapers, test fixtures — with where it came from and its
licence. Code dependencies are listed in
[`docs/THIRD_PARTY.md`](docs/THIRD_PARTY.md), imported source files in
[`PROVENANCE.md`](PROVENANCE.md).

This table and [`REUSE.toml`](REUSE.toml) must agree: `REUSE.toml` is
what tools read, this is what people read. An asset file added to the
repository needs a row here (CI checks that; see
`tools/check-branding.py`).

## Third-party assets

| Asset | Path in repository | Author / source | Licence | Modifications |
| --- | --- | --- | --- | --- |
| Noto Sans Mono, regular, 16 px, pre-rendered glyphs — the font of the console and the desktop | not in the repository: comes with the `noto-sans-mono-bitmap` crate (see `kernel/Cargo.toml`, `libs/gfx/Cargo.toml`) and is embedded in the kernel and in programs that draw text | The Noto Project Authors, <https://github.com/notofonts/latin-greek-cyrillic>; crate by Philipp Schuster, <https://github.com/phip1611/noto-sans-mono-bitmap-rs> | Font: SIL Open Font License 1.1. Crate: MIT | None by this project; the crate ships the glyphs rendered to bitmaps |

There are no third-party icons, cursors, sounds, images or wallpapers.

## Original work of the project

| Asset | Path in repository | Author | Licence | Notes |
| --- | --- | --- | --- | --- |
| Screenshot of the console | `docs/screenshot.png` | Omit OS contributors | CC-BY-SA-4.0 | original work: taken from the system running in QEMU |
| Screenshot of the desktop | `docs/screenshot-desktop.png` | Omit OS contributors | CC-BY-SA-4.0 | original work, as above |
| Screenshot of a graphical Windows test program on the desktop | `docs/screenshot-windows-gui.png` | Omit OS contributors | CC-BY-SA-4.0 | original work; the program shown is `compat/windows/samples/src/bin/wingui.rs`, written for this project |
| Window decorations, task bar, mouse pointer, the drawings in the bundled programs | drawn by code in `libs/gfx/` and `userspace/programs/` | Omit OS contributors | the licence of that code (see `COPYING`) | original work; there are no image files |
| Windows test programs and libraries (`*.exe`, `*.dll` on the system image) | built from `compat/windows/samples/`; no binaries in the repository | Omit OS contributors | MIT OR Apache-2.0 | original work, written against the public Windows API documentation |

The project has no logo, wallpaper or sound yet. When it gets one it is
listed here, as original work under CC-BY-SA-4.0 unless stated
otherwise; the name and logo are also covered by
[`TRADEMARKS.md`](TRADEMARKS.md), which a copyright licence does not
override.

Nothing here was extracted from Windows, macOS or any other proprietary
software; see [`IP_POLICY.md`](IP_POLICY.md).
