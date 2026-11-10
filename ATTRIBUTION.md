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

| Inter 4.0, weights Regular, Medium and SemiBold, drawn at 11 to 32 px as bitmap fonts — the typeface of the desktop | `libs/gfx/fonts/inter-regular-11.omf`, `libs/gfx/fonts/inter-regular-12.omf`, `libs/gfx/fonts/inter-regular-13.omf`, `libs/gfx/fonts/inter-medium-13.omf`, `libs/gfx/fonts/inter-semibold-13.omf`, `libs/gfx/fonts/inter-semibold-15.omf`, `libs/gfx/fonts/inter-semibold-20.omf`, `libs/gfx/fonts/inter-semibold-32.omf` | The Inter Project Authors, <https://github.com/rsms/inter>; taken from the Debian package `fonts-inter` 4.0 | SIL Open Font License 1.1 (`LICENSES/OFL-1.1.txt`); no reserved font name | Rendered to coverage bitmaps by `tools/gen-font.py`: printable ASCII and a few punctuation marks, arrows and arithmetic signs only, without hinting or kerning. The font files themselves are not in the repository |

There are no third-party icons, cursors, sounds, images or wallpapers.
| The certificate authorities trusted for encrypted connections: Mozilla's CA certificate list | not in the repository: comes with the `webpki-roots` crate (see `userspace/tls/Cargo.toml`) and is embedded in programs that use `omit-tls` (`/bin/fetch`) | Mozilla, via the Common CA Database (<https://www.ccadb.org/>); crate by the rustls project, <https://github.com/rustls/webpki-roots> | Community Data License Agreement – Permissive – Version 2.0 (`LICENSES/CDLA-Permissive-2.0.txt`) | None by this project; the crate ships each authority's name, public key and name constraints, as of its release |

## Original work of the project

| Asset | Path in repository | Author | Licence | Notes |
| --- | --- | --- | --- | --- |
| Screenshot of the console | `docs/screenshot.png` | Omit OS contributors | CC-BY-SA-4.0 | original work: taken from the system running in QEMU |
| Screenshots of the desktop, in light and in dark mode | `docs/screenshot-desktop.png`, `docs/screenshot-desktop-dark.png` | Omit OS contributors | CC-BY-SA-4.0 | original work, as above |
| Screenshot of a graphical Windows test program on the desktop | `docs/screenshot-windows-gui.png` | Omit OS contributors | CC-BY-SA-4.0 | original work; the program shown is `compat/windows/samples/src/bin/wingui.rs`, written for this project |
| The interface's glyph icons (about fifty line drawings on a 24-unit grid) and the Start mark, from the Omit OS design system | `libs/gfx/icons/icons.json` (the drawings, as paths), `libs/gfx/icons/glyphs.omi` (drawn at 13, 16, 20 and 24 px by `tools/gen-icons.js`) | Omit OS contributors: designed by the maintainer in the project's design system | MIT OR Apache-2.0, as the library they are built into | original work |
| The wallpapers "Dawn" (light mode) and "Dusk" (dark mode), from the Omit OS design system | drawings: `libs/gfx/art/wallpaper-dawn.svg`, `libs/gfx/art/wallpaper-dusk.svg`; drawn small for scaling up by `tools/gen-art.js`: `libs/gfx/art/wallpaper-dawn.omp`, `libs/gfx/art/wallpaper-dusk.omp` | Omit OS contributors: designed by the maintainer in the project's design system | CC-BY-SA-4.0 | original work |
| The icons of the programs (Files, Terminal, Settings, Calculator, Clock, Canvas, Player, Notes) and of a folder, from the Omit OS design system | drawings: `libs/gfx/art/app-calculator.svg`, `libs/gfx/art/app-canvas.svg`, `libs/gfx/art/app-clock.svg`, `libs/gfx/art/app-files.svg`, `libs/gfx/art/app-notes.svg`, `libs/gfx/art/app-player.svg`, `libs/gfx/art/app-settings.svg`, `libs/gfx/art/app-terminal.svg`, `libs/gfx/art/folder.svg`; drawn at 20 and 30 px by `tools/gen-art.js`: `libs/gfx/art/app-calculator-20.omp`, `libs/gfx/art/app-calculator-30.omp`, `libs/gfx/art/app-canvas-20.omp`, `libs/gfx/art/app-canvas-30.omp`, `libs/gfx/art/app-clock-20.omp`, `libs/gfx/art/app-clock-30.omp`, `libs/gfx/art/app-files-20.omp`, `libs/gfx/art/app-files-30.omp`, `libs/gfx/art/app-notes-20.omp`, `libs/gfx/art/app-notes-30.omp`, `libs/gfx/art/app-player-20.omp`, `libs/gfx/art/app-player-30.omp`, `libs/gfx/art/app-settings-20.omp`, `libs/gfx/art/app-settings-30.omp`, `libs/gfx/art/app-terminal-20.omp`, `libs/gfx/art/app-terminal-30.omp`, `libs/gfx/art/folder-20.omp`, `libs/gfx/art/folder-30.omp` | Omit OS contributors: designed by the maintainer in the project's design system | CC-BY-SA-4.0 | original work |
| Window decorations, task bar, mouse pointer, the drawings in the bundled programs | drawn by code in `libs/gfx/` and `userspace/programs/` | Omit OS contributors | the licence of that code (see `COPYING`) | original work; there are no image files |
| Windows test programs and libraries (`*.exe`, `*.dll` on the system image) | built from `compat/windows/samples/`; no binaries in the repository | Omit OS contributors | MIT OR Apache-2.0 | original work, written against the public Windows API documentation |

The Start mark — a ring with one segment omitted and the dot that is
left — serves as the project's mark on the task bar; see
[`TRADEMARKS.md`](TRADEMARKS.md). The project has no sounds yet. When
it gets some, or more artwork, it is
listed here, as original work under CC-BY-SA-4.0 unless stated
otherwise; the name and logo are also covered by
[`TRADEMARKS.md`](TRADEMARKS.md), which a copyright licence does not
override.

Nothing here was extracted from Windows, macOS or any other proprietary
software; see [`IP_POLICY.md`](IP_POLICY.md).
