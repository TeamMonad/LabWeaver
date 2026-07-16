/**
 * 数据运行模式。
 *
 * - live: 默认且生产唯一允许值，连接真实后端。
 * - fixture: 仅在开发/测试时通过 VITE_DATA_MODE=fixture 显式启用，所有请求走本地确定性 fixture。
 *
 * 任何非法值或生产构建使用 fixture 都应直接 fail-fast。
 */

export type DataMode = 'live' | 'fixture'

function parseDataMode(raw: unknown): DataMode {
  if (raw === 'fixture') return 'fixture'
  if (raw === undefined || raw === null || raw === '' || raw === 'live') return 'live'
  throw new Error(`[dataMode] 非法的 VITE_DATA_MODE=${String(raw)}，仅允许 live 或 fixture`)
}

export const DATA_MODE: DataMode = parseDataMode(import.meta.env.VITE_DATA_MODE)

export const IS_FIXTURE = DATA_MODE === 'fixture'
export const IS_LIVE = DATA_MODE === 'live'
