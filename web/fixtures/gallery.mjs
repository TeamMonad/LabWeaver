#!/usr/bin/env node

import { readdir, writeFile } from 'node:fs/promises'
import path from 'node:path'

const DESCRIPTION = 'Fixture UI预览，业务页面渲染截图，不代表真实后端验收'

function escapeHtml(value) {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;')
}

function readableTitle(filename) {
  const stem = filename.replace(/\.(?:png|jpe?g)$/i, '')
  return stem.replace(/[-_]+/g, ' ').replace(/\s+/g, ' ').trim() || filename
}

function relativeAssetUrl(filename) {
  return `./${encodeURIComponent(filename)}`
}

function renderCard(filename, index) {
  const title = readableTitle(filename)
  const safeTitle = escapeHtml(title)
  const assetUrl = relativeAssetUrl(filename)
  return `      <article class="card">
        <a class="card__link" href="${assetUrl}" target="_blank" rel="noreferrer" aria-label="${safeTitle}，点击打开原图">
          <img src="${assetUrl}" alt="${safeTitle}" loading="lazy">
        </a>
        <div class="card__caption">
          <span class="card__index">${String(index + 1).padStart(2, '0')}</span>
          <h2>${safeTitle}</h2>
          <p>${escapeHtml(filename)}</p>
        </div>
      </article>`
}

function renderGallery(filenames) {
  const cards = filenames.map(renderCard).join('\n')
  return `<!doctype html>
<html lang="zh-CN">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <meta name="description" content="${DESCRIPTION}">
    <title>Fixture UI 预览画廊</title>
    <style>
      :root {
        color-scheme: light dark;
        --page-background: #f7f8fc;
        --surface: #ffffff;
        --surface-muted: #eef1f7;
        --text: #1c1b20;
        --text-muted: #5f6068;
        --border: #d9dce5;
        --accent: #315f9b;
        --accent-soft: #dce8ff;
      }

      @media (prefers-color-scheme: dark) {
        :root {
          --page-background: #111318;
          --surface: #1b1d23;
          --surface-muted: #272a33;
          --text: #e5e2e9;
          --text-muted: #b9bac4;
          --border: #444750;
          --accent: #b1c8ff;
          --accent-soft: #263b62;
        }
      }

      * {
        box-sizing: border-box;
      }

      body {
        min-width: 320px;
        margin: 0;
        background: var(--page-background);
        color: var(--text);
        font: 16px/1.5 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      }

      main {
        width: min(1440px, calc(100% - 32px));
        margin: 0 auto;
        padding: 32px 0 48px;
      }

      header {
        margin-bottom: 24px;
      }

      h1,
      h2,
      p {
        margin: 0;
      }

      h1 {
        font-size: clamp(1.6rem, 3vw, 2.4rem);
        line-height: 1.2;
      }

      .description {
        max-width: 72ch;
        margin-top: 8px;
        color: var(--text-muted);
      }

      .count {
        display: inline-block;
        margin-top: 12px;
        padding: 4px 10px;
        border-radius: 999px;
        background: var(--accent-soft);
        color: var(--accent);
        font-size: 0.9rem;
      }

      .gallery {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(min(100%, 320px), 1fr));
        gap: 16px;
      }

      .card {
        overflow: hidden;
        border: 1px solid var(--border);
        border-radius: 16px;
        background: var(--surface);
        box-shadow: 0 4px 16px rgb(20 24 35 / 8%);
      }

      .card__link {
        display: block;
        background: var(--surface-muted);
      }

      .card__link:focus-visible {
        outline: 3px solid var(--accent);
        outline-offset: -3px;
      }

      .card img {
        display: block;
        width: 100%;
        aspect-ratio: 16 / 10;
        object-fit: contain;
      }

      .card__caption {
        display: grid;
        grid-template-columns: auto minmax(0, 1fr);
        column-gap: 10px;
        align-items: baseline;
        padding: 12px 14px 14px;
      }

      .card__index {
        grid-row: span 2;
        color: var(--accent);
        font-size: 0.85rem;
        font-variant-numeric: tabular-nums;
      }

      .card h2 {
        overflow-wrap: anywhere;
        font-size: 1rem;
        line-height: 1.35;
      }

      .card p {
        overflow-wrap: anywhere;
        color: var(--text-muted);
        font-size: 0.82rem;
      }

      @media (max-width: 520px) {
        main {
          width: min(100% - 20px, 1440px);
          padding-top: 20px;
        }

        .gallery {
          gap: 12px;
        }
      }
    </style>
  </head>
  <body>
    <main>
      <header>
        <h1>Fixture UI 预览画廊</h1>
        <p class="description">${DESCRIPTION}</p>
        <span class="count">${filenames.length} 张截图</span>
      </header>
      <section class="gallery" aria-label="Fixture UI 截图">
${cards}
      </section>
    </main>
  </body>
</html>
`
}

async function main() {
  const screenshotDirectory = process.argv[2]
  if (!screenshotDirectory || process.argv.length !== 3) {
    throw new Error('用法：node web/fixtures/gallery.mjs <截图目录>')
  }

  const entries = await readdir(screenshotDirectory, { withFileTypes: true })
  const filenames = entries
    .filter((entry) => entry.isFile() && /\.(?:png|jpe?g)$/i.test(entry.name))
    .map((entry) => entry.name)
    .sort((left, right) => left.localeCompare(right))

  if (filenames.length === 0) {
    throw new Error('截图目录中没有 PNG、JPG 或 JPEG 文件，未生成画廊。')
  }

  await writeFile(path.join(screenshotDirectory, 'index.html'), renderGallery(filenames), 'utf8')
  console.log(`已生成 ${filenames.length} 张截图的 index.html 画廊。`)
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error))
  process.exitCode = 1
})
