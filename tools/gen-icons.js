// SPDX-FileCopyrightText: 2026 Omit OS contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Renders the interface's glyph icons (libs/gfx/icons/icons.json: a
// name and an SVG path on a 24-unit grid each, from the Omit OS design
// system) into coverage masks, in libs/gfx/icons/glyphs.omi (format:
// libs/gfx/src/icon.rs). The output is committed; run this only when
// the icons change:
//
//     node tools/gen-icons.js libs/gfx/icons/icons.json libs/gfx/icons/glyphs.omi
//
// Needs Playwright with Chromium, which does the drawing: 1.6-unit
// strokes with round caps and joins, as the design system specifies.
const fs = require('fs');
const { chromium } = require('playwright');

// 13: window controls; 16: menus and status; 20, 24: buttons.
const SIZES = [13, 16, 20, 24];

(async () => {
  const [source, destination] = process.argv.slice(2);
  const icons = JSON.parse(fs.readFileSync(source, 'utf8'));
  const browser = await chromium.launch();
  const page = await browser.newPage();
  const records = [];
  for (const name of Object.keys(icons).sort()) {
    const shape = icons[name];
    // An entry is a path, or a whole SVG body for the few marks that
    // need more than one stroke width.
    const body = shape.startsWith('<')
      ? shape
      : `<path d="${shape}" fill="none" stroke="#fff" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>`;
    for (const size of SIZES) {
      await page.setViewportSize({ width: size, height: size });
      await page.setContent(
        `<body style="margin:0;background:#000"><svg width="${size}" height="${size}" viewBox="0 0 24 24" style="display:block">${body}</svg>`
      );
      // White on black: the red channel is the coverage.
      const coverage = await page.evaluate(async (size) => {
        const svg = document.querySelector('svg');
        const image = new Image();
        image.src = 'data:image/svg+xml,' + encodeURIComponent(
          svg.outerHTML.replace('<svg', '<svg xmlns="http://www.w3.org/2000/svg"'));
        await image.decode();
        const canvas = document.createElement('canvas');
        canvas.width = canvas.height = size;
        const context = canvas.getContext('2d');
        context.fillStyle = '#000';
        context.fillRect(0, 0, size, size);
        context.drawImage(image, 0, 0, size, size);
        const pixels = context.getImageData(0, 0, size, size).data;
        return Array.from({ length: size * size }, (_, i) => pixels[i * 4]);
      }, size);
      records.push({ name, size, coverage });
    }
  }
  await browser.close();

  // Header: magic, count. Record: 16-byte name, size, 3 zero bytes,
  // offset of the size*size coverage bytes. Then the coverage.
  const header = Buffer.alloc(8 + records.length * 24);
  header.write('OMI1', 0, 'latin1');
  header.writeUInt32LE(records.length, 4);
  const parts = [];
  let offset = 0;
  records.forEach((record, index) => {
    const at = 8 + index * 24;
    if (record.name.length > 16) throw new Error(`name too long: ${record.name}`);
    header.write(record.name, at, 'latin1');
    header.writeUInt8(record.size, at + 16);
    header.writeUInt32LE(offset, at + 20);
    parts.push(Buffer.from(record.coverage));
    offset += record.coverage.length;
  });
  fs.writeFileSync(destination, Buffer.concat([header, ...parts]));
  console.log(`${records.length} masks, ${8 + records.length * 24 + offset} bytes`);
})();
