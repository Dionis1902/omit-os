# ADR 0023: The desktop follows the Omit OS design system

- Status: accepted (two stages implemented)
- Date: 2026-11-10

## Context

The first desktop (ADR 0012) was drawn to no design: flat rectangles
and one fixed-width font. The maintainer has since produced a design
system for the whole interface — colour, type, spacing, corner radii,
elevation and glass materials, icons, and about twenty screens (lock
screen, desktop and task bar, Start, search, Files, window states,
arranging windows, switching between them, quick panel and
notifications, Settings, System Monitor, terminal and calculator,
Market), each in a light and a dark mode with six accent colours. Its
stated principle: familiar to people coming from Windows — task bar at
the bottom, Start, window buttons on the right, a file manager — and
visibly its own.

The design is the maintainer's work and lives outside this repository;
this ADR records how the system implements it and how far it has got.

## Decision

- **The design system is the reference for everything the desktop
  draws.** Its tokens are code (`libs/gfx/src/theme.rs`): every colour
  in both modes and all six accents, translucent tones included.
- **The drawing library grows what the design needs**, still in
  software on the firmware's framebuffer: blending, rounded rectangles
  with smooth corners, soft shadows, coverage masks
  (`libs/gfx/src/paint.rs`). Shapes are described by how much of each
  pixel they cover; nothing is drawn large and scaled down.
- **Type is Inter**, the design's typeface (SIL Open Font License). The
  system has no font rasteriser, so the glyphs are drawn on the build
  machine by `tools/gen-font.py` at the seven weight-and-size
  combinations the interface uses and built into the library as
  coverage bitmaps (`libs/gfx/fonts/`, about 80 KiB). Terminals keep
  the fixed-width font.
- **Icons are the design's line icons**, kept as paths
  (`libs/gfx/icons/icons.json`) and drawn at four sizes by
  `tools/gen-icons.js` into coverage masks. Both generators' outputs
  are committed: building the system needs neither tool.
- **Implemented in stages**, each tested like everything else. The
  first: the task bar (the Start mark, bare program icons with a dot
  under those running and a short accent bar under the one in front,
  the label of the icon under the pointer, a two-line clock), windows
  (rounded, with a shadow that is deeper for the one in front, a title
  bar with the program's icon, and one capsule holding minimise,
  maximise and close; close turns red under the pointer), minimising,
  a Start panel listing the programs, the wallpaper, and the restyled
  About and Run windows. The second: the design's pictures — its two
  wallpapers and the programs' icons, kept as the vector drawings they
  are (`libs/gfx/art/`) and drawn by `tools/gen-art.js`, the icons at
  the sizes used and the wallpapers small, to be scaled up to the
  screen once per mode; **Settings**, "Look & feel" (light or dark,
  the accent colour, task bar icons at the left or in the middle, each
  applied at once); a search field in the Start panel (typing narrows
  the list, the arrows choose, Enter starts); and a new **Files**
  window (back and up, the path as names that can be clicked, a
  sidebar of places, a list with sizes and kinds). Settings, Files and
  the controls they are made of (segmented control, swatch, card,
  switch) live in the library, where they are host-tested. The third:
  a **Calculator** (the four operations, percent, sign, backspace; by
  mouse or keyboard; decimal arithmetic on whole numbers scaled by a
  power of ten, so that 0.1 + 0.2 is 0.3); **arranging** — a window
  dragged by its title to the left or right edge of the screen takes
  that half, to the top edge all of it, with an outline shown before
  the button is let go, and dragged away again it gets its old size
  back; and what Settings chooses is **remembered**, in a small text
  file (`desktop.conf`: on the data partition of an installed system,
  where it survives a restart; otherwise in memory until the machine
  is switched off).

## What works

- 94 host tests in `libs/gfx`: the font and icon files (every
  printable character at every size; damaged files); the shapes against
  geometry (a rounded rectangle's area to within two pixels of the
  exact figure, mirror symmetry of its corners, an outline's length);
  every text and surface pairing of all twelve themes against a 4.5:1
  contrast ratio; the window manager's behaviour, old and new; that the
  fast drawing paths give exactly the pixels of the obvious ones; and
  that after 1 500 random events — pointer, keys, buttons, themes — the
  incrementally repainted screen equals one painted from scratch.
- The QEMU desktop tests operate the new desktop with the emulated
  mouse and keyboard and check screenshots: titles in the design's
  type, programs started from the task bar, a window minimised and
  brought back from its icon, maximised and restored, dragged, the
  Start panel with the row under the pointer in the accent colour, and
  the power button ending the desktop; and, since the second stage,
  dark mode chosen in Settings (the wallpaper becomes the dusk picture
  and titles are written light on dark) and back, and a folder opened
  in Files; and, since the third, another accent colour chosen and
  found in the settings file afterwards, the Files window dragged to
  the right edge and found filling the right half, and 7 × 6 = on the
  Calculator's keys followed by − 2 Enter on the keyboard.
  That the settings file is read back at the next start is tested on
  the host only (the parser), not across a restart in QEMU.

## Cost

Input-to-screen time in a release build under emulation went from an
average of 5.5 ms with the old look to 7.7 ms, and the longest update
from 161 ms to 33 ms. (The first version of the new drawing code
measured 15.3 ms; filling the insides of shapes without per-pixel work
halved that.) Debug builds are slower by the same factor as before.

## Not yet, and differences from the design

- **Most of the design's screens**: lock screen and sign-in (there are
  no users yet), the separate search screen, the window switcher and
  the picker that offers a second window for the other half after one
  was arranged, the quick panel and notifications, System
  Monitor, Market; all pages of Settings but one. In
  Files: tabs, search, the preview pane, the context menu, renaming,
  deleting and the Recycle Bin, sorting, and scrolling with the mouse
  (there are no wheel events yet); a row is opened by clicking it a
  second time, at any interval, where the design means a double click.
  The Start panel has the design's search but not its favourites row
  or letter index.
- The Calculator has no history, memory keys or scientific page, and
  shows at most twelve digits.
- **Glass is tinted, not blurred** (and the Start panel nearly opaque,
  to stay readable without blur), and nothing is animated.
- Two programs have no picture in the design and keep line icons
  (About and the game); the wallpaper is scaled up from 320 x 180
  pixels, which suits its soft shapes but is not sharp.
- Corners are circular arcs, where the design asks for continuous
  ("superellipse") corners.
- The status icons of the design's task bar (language, network, volume,
  battery) are absent: the system has nothing true to show in them.
- Windows of other programs are still fixed in size (ADR 0018), so they
  have no maximise button.
- A task bar icon finds its program's windows by their titles starting
  with its label.
