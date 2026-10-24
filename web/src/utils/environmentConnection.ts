import type { AccessGrantSchema, ConsoleKind, EnvironmentDetailSchema, EnvironmentEndpointSchema } from '@/generated/contracts'
import type { AsyncState } from '@/types/async'

/**
 * Connection readiness is separate from instance readiness: an instance can be
 * running while its connection is still being prepared (a VM guest still
 * booting, SSH not yet handshaken) or while the caller holds no usable access
 * grant. Instance and connection readiness come only from the server's
 * `connection` read model; the browser never infers them from endpoints or the
 * runtime kind, and it fails closed when a read is missing.
 */
export type ConnectionReadinessKind =
  | 'instance-not-ready'
  | 'connection-preparing'
  | 'unknown'
  | 'connection-unavailable'
  | 'checking'
  | 'needs-grant'
  | 'needs-regrant'
  | 'grant-pending'
  | 'ready'

export interface ConnectionReadiness {
  kind: ConnectionReadinessKind
  /** Short state shown next to the instance state. */
  label: string
  /** One sentence telling the user what happens next. */
  detail: string
}

export type ConnectionFacts = Pick<EnvironmentDetailSchema, 'desiredState' | 'connection'>

/** A grant the server reported active stops counting at its own expiry, even before the next read. */
export function grantIsUsable(grant: AsyncState<AccessGrantSchema>, now: number): boolean {
  return grant.kind === 'success' && grant.data.state === 'active' && Date.parse(grant.data.expiresAt) > now
}

export function connectionReadiness(
  instance: ConnectionFacts,
  grant: AsyncState<AccessGrantSchema>,
  instanceBlockReason: string,
  now: number,
): ConnectionReadiness {
  const { connection } = instance
  // A stop or delete request ends connections, whatever the last observation said.
  if (instance.desiredState !== 'running' || !connection.instanceReady) {
    return { kind: 'instance-not-ready', label: '不可连接', detail: instanceBlockReason }
  }
  switch (connection.state) {
    case 'preparing':
      return {
        kind: 'connection-preparing',
        label: '连接准备中',
        detail: '实例已运行，平台正在准备连接入口：系统或服务可能还在启动。页面会自动重新检查，准备好后即可签发访问授权。',
      }
    case 'unknown':
      return {
        kind: 'unknown',
        label: '无法确认',
        detail: '实例已运行，但平台还没有确认连接入口可用（例如虚拟机的 SSH 还没有完成握手）。页面会自动重新检查。',
      }
    case 'unavailable':
      return {
        kind: 'connection-unavailable',
        label: '入口不可用',
        detail: '实例已运行，但连接入口当前不可用。页面会自动重新检查；持续不可用时到“异步操作与诊断”查看原因，或重启环境。',
      }
    case 'ready':
      break
  }
  if (grant.kind === 'loading') {
    return { kind: 'checking', label: '检查中', detail: '正在读取当前访问授权。' }
  }
  if (grant.kind === 'success') {
    if (grant.data.state === 'requested') {
      return { kind: 'grant-pending', label: '授权生效中', detail: '访问授权已提交，正在等待生效。' }
    }
    if (grantIsUsable(grant, now)) {
      return { kind: 'ready', label: '可连接', detail: '访问授权已生效，可以使用下方的连接方式。' }
    }
    if (grant.data.state === 'active') {
      return { kind: 'needs-regrant', label: '待重新授权', detail: '访问授权已过期，当前连接会断开。重新签发访问授权后再连接。' }
    }
  }
  if (grant.kind !== 'idle' && grant.kind !== 'success' && grant.kind !== 'empty') {
    return {
      kind: 'needs-regrant',
      label: '待重新授权',
      detail: `无法确认当前访问授权：${grant.diagnostic.message} 重新签发访问授权后再连接。`,
    }
  }
  return { kind: 'needs-grant', label: '待授权', detail: '连接入口已就绪。签发访问授权后即可连接。' }
}

export type ConnectionMethodId = 'console' | 'desktop' | 'ssh' | 'app'

export interface ConnectionMethod {
  id: ConnectionMethodId
  label: string
  description: string
}

/**
 * The ways this environment can be reached, taken only from server facts:
 * browser terminal and desktop from the console kinds the access grant
 * permits, SSH and applications from the protocols of registered endpoints.
 * Nothing is assumed from the runtime kind.
 */
export function connectionMethods(
  endpoints: AsyncState<EnvironmentEndpointSchema[]>,
  consoleKinds: readonly ConsoleKind[],
): ConnectionMethod[] {
  const methods: ConnectionMethod[] = []
  if (consoleKinds.includes('xterm')) {
    methods.push({ id: 'console', label: 'Web 终端', description: '在浏览器中打开命令行。' })
  }
  if (consoleKinds.includes('novnc')) {
    methods.push({ id: 'desktop', label: '桌面', description: '在浏览器中打开图形桌面。' })
  }
  const protocols = new Set(endpoints.kind === 'success'
    ? endpoints.data.filter((endpoint) => endpoint.health !== 'removed').map((endpoint) => endpoint.protocol)
    : [])
  if (protocols.has('ssh')) {
    methods.push({ id: 'ssh', label: 'SSH', description: '签发授权后显示一行 SSH 命令，复制到本机终端执行。' })
  }
  if (protocols.has('http') || protocols.has('https')) {
    methods.push({ id: 'app', label: '应用', description: '签发授权后通过当前登录会话打开环境中的 Web 应用。' })
  }
  return methods
}
