// Generate every application-icon format from one SVG source.
//
// Usage:
//   npm run icons        Write/update generated assets.
//   npm run icons:check  Fail when generated assets are missing or stale.
//   node scripts/generate-icons.mjs --write \
//     --root <project-root> --source <logo.svg> --output <icons-dir> \
//     --name <file-stem> --app-id <desktop-app-id>
//
// With no path/name options, the original Termior paths remain the defaults.
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import * as png2icons from "png2icons";
import sharp from "sharp";

const SCRIPT_ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const DEFAULTS = {
  root: SCRIPT_ROOT,
  source: join("assets", "termior-logo.svg"),
  output: join("assets", "icons"),
  name: "termior",
  appId: "app.termior.Termior",
};

const PNG_SIZES = [16, 24, 32, 48, 64, 128, 256, 512, 1024];
const LINUX_SIZES = PNG_SIZES.filter((size) => size <= 512);
const MASTER_SIZE = 1024;

function parseArgs(args) {
  const options = { ...DEFAULTS };
  let mode = "--write";

  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--write" || argument === "--check") {
      mode = argument;
      continue;
    }

    if (!["--root", "--source", "--output", "--name", "--app-id"].includes(argument)) {
      throw new Error(`unknown option ${argument}`);
    }

    const value = args[index + 1];
    if (!value || value.startsWith("--")) {
      throw new Error(`${argument} requires a value`);
    }
    index += 1;

    if (argument === "--app-id") {
      options.appId = value;
    } else {
      options[argument.slice(2)] = value;
    }
  }

  const root = resolve(options.root);
  const resolveFromRoot = (path) =>
    isAbsolute(path) ? resolve(path) : resolve(root, path);

  if (!/^[A-Za-z0-9._-]+$/.test(options.name)) {
    throw new Error("--name must be a safe file name without path separators");
  }
  if (!/^[A-Za-z0-9._-]+$/.test(options.appId)) {
    throw new Error("--app-id contains unsupported characters");
  }

  return {
    mode,
    root,
    source: resolveFromRoot(options.source),
    outputDir: resolveFromRoot(options.output),
    name: options.name,
    appId: options.appId,
  };
}

function displayPath(root, path) {
  return relative(root, path).replaceAll("\\", "/");
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

async function buildOutputs(config) {
  const svg = await readFile(config.source);
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
    [join(config.outputDir, `${config.name}.png`), pngBySize.get(512)],
    [join(config.outputDir, `${config.name}.ico`), ico],
    [join(config.outputDir, `${config.name}.icns`), icns],
  ]);

  for (const size of PNG_SIZES) {
    outputs.set(
      join(config.outputDir, "png", `${size}x${size}.png`),
      pngBySize.get(size),
    );
  }

  for (const size of LINUX_SIZES) {
    outputs.set(
      join(
        config.outputDir,
        "hicolor",
        `${size}x${size}`,
        "apps",
        `${config.appId}.png`,
      ),
      pngBySize.get(size),
    );
  }

  outputs.set(
    join(
      config.outputDir,
      "hicolor",
      "scalable",
      "apps",
      `${config.appId}.svg`,
    ),
    svg,
  );

  return outputs;
}

async function writeOutputs(config, outputs) {
  for (const [path, contents] of outputs) {
    await mkdir(dirname(path), { recursive: true });
    await writeFile(path, contents);
    console.log(`wrote ${displayPath(config.root, path)}`);
  }
}

async function checkOutputs(config, outputs) {
  const stale = [];

  for (const [path, expected] of outputs) {
    try {
      const actual = await readFile(path);
      if (!actual.equals(expected)) {
        stale.push(`${displayPath(config.root, path)} is stale`);
      }
    } catch (error) {
      if (error?.code === "ENOENT") {
        stale.push(`${displayPath(config.root, path)} is missing`);
      } else {
        throw error;
      }
    }
  }

  if (stale.length > 0) {
    throw new Error(
      `generated icons are not up to date:\n- ${stale.join("\n- ")}\nRun the generator with the same options and --write.`,
    );
  }

  console.log(`verified ${outputs.size} generated icon assets`);
}

async function main() {
  const config = parseArgs(process.argv.slice(2));
  const outputs = await buildOutputs(config);
  if (config.mode === "--check") {
    await checkOutputs(config, outputs);
  } else {
    await writeOutputs(config, outputs);
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
});
