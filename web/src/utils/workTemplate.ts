import type { EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'

type RuntimeKind = EnvironmentTemplateReleaseViewSchema['runtimeKind']

export function runtimeKindLabel(kind: RuntimeKind): string {
  return kind === 'virtual_machine' ? '虚拟机' : '容器'
}

/** Readable title based on runtime kind and the immutable version number. */
export function workTemplateTitle(release: Pick<EnvironmentTemplateReleaseViewSchema, 'runtimeKind' | 'version'>): string {
  return `${runtimeKindLabel(release.runtimeKind)}模板 v${release.version}`
}

function shortDigest(digest: string): string {
  const [algorithm, value] = digest.includes(':') ? digest.split(':', 2) : ['sha256', digest]
  return `${algorithm}:${value.slice(0, 12)}`
}

/** One line describing the exact image the template pins. */
export function workTemplateArtifactLabel(release: Pick<EnvironmentTemplateReleaseViewSchema, 'artifact'>): string {
  const artifact = release.artifact
  if (artifact.kind === 'container' || artifact.kind === 'catalog_container') {
    const repository = artifact.repository.split('/').slice(-2).join('/')
    return `容器镜像 ${repository} · ${shortDigest(artifact.digest)}`
  }
  const capacity = artifact.base_disk.capacityBytes
  const size = typeof capacity === 'number' && capacity > 0 ? ` · ${Math.round(capacity / 1024 ** 3)} GiB` : ''
  return `虚拟机磁盘 · ${artifact.format}${size}`
}

export function isUsableRelease(release: Pick<EnvironmentTemplateReleaseViewSchema, 'withdrawal'>): boolean {
  return !release.withdrawal
}
