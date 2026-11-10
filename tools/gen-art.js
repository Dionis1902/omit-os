// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Renders the pictures of the Omit OS design system (libs/gfx/art/*.svg:
// the wallpapers and the programs' icons) into the plain pixel files the
// system can draw (libs/gfx/art/*.omp; format: libs/gfx/src/art.rs).
// The outputs are committed; run this only when a picture changes:
//
//     node tools/gen-art.js libs/gfx/art
//
// Needs Playwright with Chromium, which does the drawing.
//
// Icons are drawn at the sizes the interface uses, with the soft shadow
// the design system puts under every icon, and keep their transparency.
// Wallpapers are drawn small and opaque: the system scales them up to
// the screen, which suits pictures made of soft gradients and keeps
// megabytes out of the system image.
const fs = require('fs');
const path = require('path');
const { chromium } = require('playwright');

const ICON_SIZES = [20, 30];
const WALLPAPER = [320, 180];

(async () => {
  const directory = process.argv[2];
  const browser = await chromium.launch();
  const page = await browser.newPage();
  // Draws `svg` scaled to `width` x `height` and returns its pixels,
  // four bytes each: red, green, blue, opacity.
  const draw = (svg, width, height, shadow) =>
    page.evaluate(
      async ({ svg, width, height, shadow }) => {
        const image = new Image();
        image.src = 'data:image/svg+xml,' + encodeURIComponent(svg);
        await image.decode();
        const canvas = document.createElement('canvas');
        canvas.width = width;
        canvas.height = height;
        const context = canvas.getContext('2d');
        if (shadow) {
          // "0 1px 1.5px black at 14%", for an icon drawn 30 px wide.
          context.shadowColor = 'rgba(0,0,0,0.14)';
          context.shadowOffsetY = width / 30;
          context.shadowBlur = (1.5 * width) / 30;
        }
        context.drawImage(image, 0, 0, width, height);
        return Array.from(context.getImageData(0, 0, width, height).data);
      },
      { svg, width, height, shadow }
    );
  // File: "OMP1", width (u16), height (u16), bytes per pixel (u8: 3 or
  // 4), three zero bytes, then the pixels row by row.
  const write = (name, width, height, channels, pixels) => {
    const header = Buffer.alloc(12);
    header.write('OMP1', 0, 'latin1');
    header.writeUInt16LE(width, 4);
    header.writeUInt16LE(height, 6);
    header.writeUInt8(channels, 8);
    const body = Buffer.alloc(width * height * channels);
    for (let i = 0; i < width * height; i++) {
      for (let c = 0; c < channels; c++) body[i * channels + c] = pixels[i * 4 + c];
    }
    fs.writeFileSync(path.join(directory, name), Buffer.concat([header, body]));
    console.log(`${name}: ${12 + body.length} bytes`);
  };
  for (const file of fs.readdirSync(directory).sort()) {
    if (!file.endsWith('.svg')) continue;
    const svg = fs.readFileSync(path.join(directory, file), 'utf8');
    const name = file.slice(0, -4);
    if (name.startsWith('wallpaper-')) {
      const [width, height] = WALLPAPER;
      write(`${name}.omp`, width, height, 3, await draw(svg, width, height, false));
    } else {
      for (const size of ICON_SIZES) {
        write(`${name}-${size}.omp`, size, size, 4, await draw(svg, size, size, true));
      }
    }
  }
  await browser.close();
})();
