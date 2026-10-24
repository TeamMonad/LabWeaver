import type { EnvironmentTemplateReleaseViewSchema, TemplateCapability } from '@/generated/contracts'

type RuntimeKind = EnvironmentTemplateReleaseViewSchema['runtimeKind']
type Release = EnvironmentTemplateReleaseViewSchema

export function runtimeKindLabel(kind: RuntimeKind): string {
  return kind === 'virtual_machine' ? '虚拟机' : '容器'
}

/** The template's published name and its immutable version number. */
export function workTemplateTitle(release: Pick<Release, 'name' | 'runtimeKind' | 'version'>): string {
  const name = release.name.trim()
  return `${name || `${runtimeKindLabel(release.runtimeKind)}模板`} v${release.version}`
}

/** `algorithm:` plus the first 12 hex digits, enough to tell pinned images apart. */
export function shortDigest(digest: string): string {
  const [algorithm, value] = digest.includes(':') ? digest.split(':', 2) : ['sha256', digest]
  return `${algorithm}:${value.slice(0, 12)}`
}

/** One line describing the exact image the template pins. */
export function workTemplateArtifactLabel(release: Pick<Release, 'artifact'>): string {
  const artifact = release.artifact
  if (artifact.kind === 'container' || artifact.kind === 'catalog_container') {
    const repository = artifact.repository.split('/').slice(-2).join('/')
    return `容器镜像 ${repository} · ${shortDigest(artifact.digest)}`
  }
  const capacity = artifact.base_disk.capacityBytes
  const size = typeof capacity === 'number' && capacity > 0 ? ` · ${Math.round(capacity / 1024 ** 3)} GiB` : ''
  return `虚拟机磁盘 · ${artifact.format}${size}`
}

/** Who reviewed the template: a person directly, or an Agent candidate a person approved. */
export function workTemplateSourceLabel(release: Pick<Release, 'manualApproval'>): string {
  return release.manualApproval ? '手工发布' : 'Agent 生成并审核'
}

/** A release that can still create new Work: published for Work and not withdrawn. */
export function isWorkTemplate(release: Pick<Release, 'purpose' | 'withdrawal'>): boolean {
  return release.purpose === 'work' && !release.withdrawal
}

const CAPABILITY_LABELS: Record<TemplateCapability, string> = {
  terminal: '浏览器终端',
  desktop: '桌面',
  ssh: 'SSH',
  applications: 'Web 应用',
  files: '文件',
  agent: 'Agent',
}

export function templateCapabilityLabel(capability: TemplateCapability): string {
  return CAPABILITY_LABELS[capability]
}

/**
 * Newer Work releases offered as a new Work for this environment: the same
 * project, course, template family and runtime, a higher version, not
 * withdrawn. Newest first.
 */
export function newerWorkReleases(
  releases: readonly Release[],
  current: Pick<Release, 'templateFamilyId' | 'runtimeKind' | 'version' | 'projectId' | 'courseId'>,
): Release[] {
  return releases
    .filter((release) => (
      isWorkTemplate(release)
      && release.projectId === current.projectId
      && (release.courseId ?? null) === (current.courseId ?? null)
      && release.templateFamilyId === current.templateFamilyId
      && release.runtimeKind === current.runtimeKind
      && release.version > current.version
    ))
    .sort((left, right) => right.version - left.version)
}
