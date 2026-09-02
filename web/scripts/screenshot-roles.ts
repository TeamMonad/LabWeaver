import { chromium, type Browser, type Page } from '@playwright/test'
import { createServer } from 'vite'
import type { ViteDevServer } from 'vite'
import { resolve, dirname } from 'path'
import { fileURLToPath } from 'url'

const __dirname = dirname(fileURLToPath(import.meta.url))

async function withDevServer<T>(fn: (url: string) => Promise<T>): Promise<T> {
  const server: ViteDevServer = await createServer({
    root: resolve(__dirname, '..'),
    server: { port: 0 },
  })
  await server.listen()
  const address = server.httpServer?.address()
  const port = typeof address === 'object' && address !== null ? address.port : 5173
  const url = `http://localhost:${port}`
  try {
    return await fn(url)
  } finally {
    await server.close()
  }
}

async function screenshotPage(page: Page, url: string, path: string): Promise<void> {
  await page.goto(url)
  await page.setViewportSize({ width: 1280, height: 800 })
  await page.waitForLoadState('networkidle')
  await page.screenshot({ path, fullPage: false })
}

async function main(): Promise<void> {
  const browser: Browser = await chromium.launch()
  const page = await browser.newPage()

  await withDevServer(async (url) => {
    await screenshotPage(page, url, resolve(__dirname, '../screenshots/home.png'))
    await screenshotPage(page, `${url}/teacher`, resolve(__dirname, '../screenshots/teacher.png'))
    await screenshotPage(page, `${url}/student`, resolve(__dirname, '../screenshots/student.png'))
    await screenshotPage(page, `${url}/researcher`, resolve(__dirname, '../screenshots/researcher.png'))
    await screenshotPage(page, `${url}/admin`, resolve(__dirname, '../screenshots/admin.png'))
  })

  await browser.close()
  console.log('Screenshots saved to screenshots/')
}

main().catch((err) => {
  console.error(err)
  process.exit(1)
})
