// Generate every application-icon format from assets/termior-logo.svg.
//
// Usage:
//   npm run icons        Write/update generated assets.
//   npm run icons:check  Fail when generated assets are missing or stale.

// Outputs:
//   assets/icons/termior.png                 512px preview/general-purpose PNG
//   assets/icons/termior.ico                 Windows executable/window icon
//   assets/icons/termior.icns                macOS application bundle icon
//   assets/icons/png/<size>x<size>.png        Cross-platform raster sizes
//   assets/icons/hicolor/...                 Linux freedesktop icon theme assets
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join, relative } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import * as png2icons from "png2icons";
import sharp from "sharp";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const SOURCE = join(ROOT, "assets", "termior-logo.svg");
const OUTPUT_DIR = join(ROOT, "assets", "icons");
const APP_ID = "app.termior.Termior";

const PNG_SIZES = [16, 24, 32, 48, 64, 128, 256, 512, 1024];
const LINUX_SIZES = PNG_SIZES.filter((size) => size <= 512);
const MASTER_SIZE = 1024;

function displayPath(path) {
  return relative(ROOT, path).replaceAll("\\", "/");
}

async function renderPng(svg, size) {
  return sharp(svg, { density: 384 })
    .resize(size, size, {
      fit: "contain",
      background: { r: 0, g: 0, b: 0, alpha: 0 },
    })
    .png({ compressionLevel: 9, adaptiveFiltering: true })
    .toBuffer();
}

function requireIconBuffer(buffer, format) {
  if (!Buffer.isBuffer(buffer) || buffer.length === 0) {
    throw new Error(`png2icons failed to create the ${format} icon`);
  }
  return buffer;
}

function validateIco(buffer) {
  const expectedSizes = [16, 24, 32, 48, 64, 72, 96, 128, 256];
  const imageCount = buffer.readUInt16LE(4);
  const actualSizes = [];

  if (buffer.readUInt16LE(0) !== 0 || buffer.readUInt16LE(2) !== 1) {
    throw new Error("generated Windows icon has an invalid ICO header");
  }
  if (buffer.length < 6 + imageCount * 16) {
    throw new Error("generated Windows icon has a truncated image directory");
  }

  for (let index = 0; index < imageCount; index += 1) {
    const offset = 6 + index * 16;
    actualSizes.push(buffer[offset] || 256);
  }
  actualSizes.sort((left, right) => left - right);

  if (actualSizes.join(",") !== expectedSizes.join(",")) {
    throw new Error(
      `generated Windows icon has unexpected sizes: ${actualSizes.join(", ")}`,
    );
  }
}

function validateIcns(buffer) {
  if (buffer.toString("ascii", 0, 4) !== "icns") {
    throw new Error("generated macOS icon has an invalid ICNS header");
  }
  if (buffer.readUInt32BE(4) !== buffer.length) {
    throw new Error("generated macOS icon has an invalid container length");
  }
}

async function buildOutputs() {
  const svg = await readFile(SOURCE);
  const pngBySize = new Map();

  await Promise.all(
    PNG_SIZES.map(async (size) => {
      pngBySize.set(size, await renderPng(svg, size));
    }),
  );

  const masterPng = pngBySize.get(MASTER_SIZE);
  const ico = requireIconBuffer(
    png2icons.createICO(masterPng, png2icons.BICUBIC, 0, false, true),
    "Windows ICO",
  );
  const icns = requireIconBuffer(
    png2icons.createICNS(masterPng, png2icons.BICUBIC, 0),
    "macOS ICNS",
  );
  validateIco(ico);
  validateIcns(icns);

  const outputs = new Map([
    [join(OUTPUT_DIR, "termior.png"), pngBySize.get(512)],
    [join(OUTPUT_DIR, "termior.ico"), ico],
    [join(OUTPUT_DIR, "termior.icns"), icns],
  ]);

  for (const size of PNG_SIZES) {
    outputs.set(
      join(OUTPUT_DIR, "png", `${size}x${size}.png`),
      pngBySize.get(size),
    );
  }

  for (const size of LINUX_SIZES) {
    outputs.set(
      join(
        OUTPUT_DIR,
        "hicolor",
        `${size}x${size}`,
        "apps",
        `${APP_ID}.png`,
      ),
      pngBySize.get(size),
    );
  }

  outputs.set(
    join(OUTPUT_DIR, "hicolor", "scalable", "apps", `${APP_ID}.svg`),
    svg,
  );

  return outputs;
}

async function writeOutputs(outputs) {
  for (const [path, contents] of outputs) {
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, contents);
    console.log(`wrote ${displayPath(path)}`);
  }
}

async function checkOutputs(outputs) {
  const stale = [];

  for (const [path, expected] of outputs) {
    try {
      const actual = await readFile(path);
      if (!actual.equals(expected)) {
        stale.push(`${displayPath(path)} is stale`);
      }
    } catch (error) {
      if (error?.code === "ENOENT") {
        stale.push(`${displayPath(path)} is missing`);
      } else {
        throw error;
      }
    }
  }

  if (stale.length > 0) {
    throw new Error(
      `generated icons are not up to date:\n- ${stale.join("\n- ")}\nRun \`npm run icons\`.`,
    );
  }

  console.log(`verified ${outputs.size} generated icon assets`);
}

async function main() {
  const mode = process.argv[2] ?? "--write";
  if (mode !== "--write" && mode !== "--check") {
    throw new Error(`unknown option ${mode}; expected --write or --check`);
  }

  const outputs = await buildOutputs();
  if (mode === "--check") {
    await checkOutputs(outputs);
  } else {
    await writeOutputs(outputs);
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
});
