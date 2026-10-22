import type { AccessGrantSchema, EnvironmentEndpointSchema, EnvironmentInstanceSchema } from '@/generated/contracts'
import type { AsyncState } from '@/types/async'

/**
 * Connection readiness is separate from instance readiness: an instance can be
 * running while its endpoints are still being checked (a VM guest still
 * booting, a service not yet listening) or while the caller holds no access
 * grant. Every input here is a server read; the browser only orders them.
 */
export type ConnectionReadinessKind =
  | 'instance-not-ready'
  | 'checking'
  | 'unknown'
  | 'no-endpoints'
  | 'endpoints-pending'
  | 'endpoints-unhealthy'
  | 'needs-grant'
  | 'grant-pending'
  | 'ready'

export interface ConnectionReadiness {
  kind: ConnectionReadinessKind
  /** Short state shown next to the instance state. */
  label: string
  /** One sentence telling the user what happens next. */
  detail: string
}

export function instanceIsReady(instance: Pick<EnvironmentInstanceSchema, 'desiredState' | 'observedState'>): boolean {
  return instance.desiredState === 'running' && instance.observedState === 'ready'
}

export function connectionReadiness(
  instance: Pick<EnvironmentInstanceSchema, 'desiredState' | 'observedState'>,
  endpoints: AsyncState<EnvironmentEndpointSchema[]>,
  grant: AsyncState<AccessGrantSchema>,
  instanceBlockReason: string,
): ConnectionReadiness {
  if (!instanceIsReady(instance)) {
    return { kind: 'instance-not-ready', label: '不可连接', detail: instanceBlockReason }
  }
  if (endpoints.kind === 'idle' || endpoints.kind === 'loading') {
    return { kind: 'checking', label: '检查中', detail: '正在读取连接入口。' }
  }
  if (endpoints.kind === 'empty') {
    return {
      kind: 'no-endpoints',
      label: '暂无入口',
      detail: '实例已运行，但平台还没有为它登记连接入口。请稍后刷新；持续没有入口时到“异步操作与诊断”查看原因。',
    }
  }
  if (endpoints.kind !== 'success') {
    return { kind: 'unknown', label: '无法确认', detail: `无法读取连接入口：${endpoints.diagnostic.message}` }
  }
  const live = endpoints.data.filter((endpoint) => endpoint.health !== 'removed')
  if (live.length === 0) {
    return { kind: 'no-endpoints', label: '暂无入口', detail: '连接入口已全部移除，请刷新或重启环境。' }
  }
  if (!live.some((endpoint) => endpoint.health === 'healthy')) {
    return live.some((endpoint) => endpoint.health === 'pending')
      ? {
          kind: 'endpoints-pending',
          label: '入口检查中',
          detail: '实例已运行，连接入口仍在检查：系统或服务可能还在启动。请稍候刷新，入口健康后即可签发访问授权。',
        }
      : {
          kind: 'endpoints-unhealthy',
          label: '入口未就绪',
          detail: '实例已运行，但连接入口检查未通过，暂时无法连接。可以刷新重试，或重启环境。',
        }
  }
  if (grant.kind === 'loading') {
    return { kind: 'checking', label: '检查中', detail: '正在读取当前访问授权。' }
  }
  if (grant.kind === 'success') {
    if (grant.data.state === 'active') {
      return { kind: 'ready', label: '可连接', detail: '访问授权已生效，可以打开下方的连接方式。' }
    }
    if (grant.data.state === 'requested') {
      return { kind: 'grant-pending', label: '授权生效中', detail: '访问授权已提交，正在等待生效。' }
    }
  }
  return { kind: 'needs-grant', label: '待授权', detail: '入口已就绪。签发访问授权后即可连接。' }
}

export type ConnectionMethodId = 'console' | 'desktop' | 'ssh' | 'app'

export interface ConnectionMethod {
  id: ConnectionMethodId
  label: string
  description: string
}

/**
 * The ways this environment can be reached, derived from its runtime and the
 * protocols of the endpoints the server registered. Nothing is offered that
 * the environment does not expose.
 */
export function connectionMethods(
  instance: Pick<EnvironmentInstanceSchema, 'runtimeKind'>,
  endpoints: AsyncState<EnvironmentEndpointSchema[]>,
): ConnectionMethod[] {
  const methods: ConnectionMethod[] = instance.runtimeKind === 'virtual_machine'
    ? [{ id: 'desktop', label: '桌面', description: '在浏览器中打开虚拟机图形桌面。' }]
    : [{ id: 'console', label: 'Web 终端', description: '在浏览器中打开容器命令行。' }]
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
