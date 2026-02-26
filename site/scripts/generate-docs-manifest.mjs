#!/usr/bin/env node

import fs from "node:fs/promises";
import path from "node:path";

const siteDir = process.cwd();
const repoRoot = path.resolve(siteDir, "..");
const docsRoot = path.join(repoRoot, "docs");
const outRoot = path.join(siteDir, "public", "docs-content");
const generatedDir = path.join(siteDir, "src", "generated");
const manifestFile = path.join(generatedDir, "docs-manifest.json");

const ROOT_ASSET_PATTERN = /\.(png|jpe?g|gif|webp|svg|avif)$/i;
const MARKDOWN_PATTERN = /\.(md|mdx)$/i;

function toPosix(filePath) {
  return filePath.replace(/\\/g, "/");
}

function normalizePath(filePath) {
  return toPosix(filePath).replace(/^\/+/, "").replace(/\/+/g, "/");
}

function stripMarkdownSyntax(text) {
  return text
    .replace(/`([^`]+)`/g, "$1")
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/<[^>]+>/g, "")
    .replace(/[*_~>#]+/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

function detectLanguage(relativePath) {
  const rel = normalizePath(relativePath);

  const i18nMatch = /^docs\/i18n\/([^/]+)\//.exec(rel);
  if (i18nMatch) {
    return i18nMatch[1];
  }

  const suffixMatch = /\.(zh-CN|ja|ru|fr|vi|el)\.(md|mdx)$/i.exec(rel);
  if (suffixMatch) {
    return suffixMatch[1];
  }

  return "en";
}

function detectSection(relativePath) {
  const rel = normalizePath(relativePath);

  if (!rel.startsWith("docs/")) {
    return "root";
  }

  const parts = rel.split("/");

  if (parts[1] === "i18n") {
    return parts[2] ? `i18n/${parts[2]}` : "i18n";
  }

  return parts[1] || "docs";
}

function fallbackTitle(relativePath) {
  const filename = path.basename(relativePath).replace(/\.(md|mdx)$/i, "");

  if (filename.toLowerCase() === "readme") {
    const parent = path.basename(path.dirname(relativePath));
    if (parent && parent !== "." && parent !== "docs" && parent !== "i18n") {
      return `${parent} README`;
    }
  }

  return filename
    .replace(/[._-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
}

function extractTitle(markdown, relativePath) {
  const lines = markdown.split(/\r?\n/);

  for (const line of lines) {
    const trimmed = line.trim();
    const heading = /^#{1,2}\s+(.+)$/.exec(trimmed);
    if (heading) {
      const title = stripMarkdownSyntax(heading[1].replace(/\s+#*$/, ""));
      if (title) return title;
    }
  }

  const h1Tag = /<h1[^>]*>([\s\S]*?)<\/h1>/i.exec(markdown);
  if (h1Tag) {
    const title = stripMarkdownSyntax(h1Tag[1]);
    if (title) return title;
  }

  return fallbackTitle(relativePath);
}

function extractSummary(markdown) {
  const lines = markdown.split(/\r?\n/);
  let inCode = false;

  for (const rawLine of lines) {
    const line = rawLine.trim();

    if (line.startsWith("```")) {
      inCode = !inCode;
      continue;
    }

    if (inCode || !line) {
      continue;
    }

    if (
      line.startsWith("#") ||
      line.startsWith("|") ||
      line.startsWith("<") ||
      line.startsWith(">") ||
      line.startsWith("-") ||
      line.startsWith("*")
    ) {
      continue;
    }

    const cleaned = stripMarkdownSyntax(line);
    if (cleaned.length >= 24) {
      return cleaned.slice(0, 220);
    }
  }

  return "Project documentation.";
}

function toId(relativePath) {
  return normalizePath(relativePath)
    .toLowerCase()
    .replace(/[^a-z0-9/.-]/g, "-")
    .replace(/[/.]+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
}

async function ensureDir(dirPath) {
  await fs.mkdir(dirPath, { recursive: true });
}

async function walkFiles(rootDir) {
  const result = [];
  const stack = [rootDir];

  while (stack.length > 0) {
    const current = stack.pop();
    if (!current) continue;

    const entries = await fs.readdir(current, { withFileTypes: true });

    for (const entry of entries) {
      const next = path.join(current, entry.name);
      if (entry.isDirectory()) {
        stack.push(next);
      } else if (entry.isFile()) {
        result.push(next);
      }
    }
  }

  return result;
}

async function copyIntoPublic(filePath) {
  const rel = normalizePath(path.relative(repoRoot, filePath));
  const target = path.join(outRoot, rel);
  await ensureDir(path.dirname(target));
  await fs.copyFile(filePath, target);
}

async function main() {
  await ensureDir(generatedDir);
  await fs.rm(outRoot, { recursive: true, force: true });
  await ensureDir(outRoot);

  const rootEntries = await fs.readdir(repoRoot, { withFileTypes: true });
  const rootMarkdownFiles = rootEntries
    .filter((entry) => entry.isFile() && MARKDOWN_PATTERN.test(entry.name))
    .map((entry) => path.join(repoRoot, entry.name));

  const rootAssetFiles = rootEntries
    .filter((entry) => entry.isFile() && ROOT_ASSET_PATTERN.test(entry.name))
    .map((entry) => path.join(repoRoot, entry.name));

  const docsAllFiles = await walkFiles(docsRoot);
  const markdownDocs = docsAllFiles.filter((filePath) => MARKDOWN_PATTERN.test(filePath));

  for (const filePath of docsAllFiles) {
    await copyIntoPublic(filePath);
  }

  for (const filePath of rootMarkdownFiles) {
    await copyIntoPublic(filePath);
  }

  for (const filePath of rootAssetFiles) {
    await copyIntoPublic(filePath);
  }

  const manifestEntries = [];
  const markdownFiles = [...rootMarkdownFiles, ...markdownDocs];

  for (const filePath of markdownFiles) {
    const relativePath = normalizePath(path.relative(repoRoot, filePath));
    const content = await fs.readFile(filePath, "utf8");

    manifestEntries.push({
      id: toId(relativePath),
      path: relativePath,
      title: extractTitle(content, relativePath),
      summary: extractSummary(content),
      section: detectSection(relativePath),
      language: detectLanguage(relativePath),
      sourceUrl: `https://github.com/zeroclaw-labs/zeroclaw/blob/main/${relativePath}`,
    });
  }

  manifestEntries.sort((a, b) => a.path.localeCompare(b.path));

  await fs.writeFile(manifestFile, JSON.stringify(manifestEntries, null, 2) + "\n", "utf8");

  process.stdout.write(
    `[docs-manifest] generated ${manifestEntries.length} markdown entries and copied docs assets\n`
  );
}

main().catch((error) => {
  process.stderr.write(`[docs-manifest] generation failed: ${String(error)}\n`);
  process.exit(1);
});
