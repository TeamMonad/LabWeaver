import type { EnvironmentTemplateReleaseViewSchema } from '@/generated/contracts'
import { listTemplateReleases } from '../stores/templateReleaseStore'
import { extractPathParam, requireActor } from './index'
import type { FixtureHandler } from '../types'

function parseCourseId(url: string): string | null {
  return extractPathParam(url, /^\/api\/v1\/courses\/([^/]+)\/environment-template-releases$/, 1)
}

export const listEnvironmentTemplateReleases: FixtureHandler = (req) => {
  const actorResult = requireActor(req)
  if (!('role' in actorResult)) return actorResult

  const courseId = parseCourseId(req.url)
  if (!courseId) {
    return {
      status: 400,
      data: {
        type: 'about:blank',
        title: 'Bad Request',
        status: 400,
        detail: '无效的课程 ID',
        instance: req.url,
        diagnosticCode: 'FIXTURE_INVALID_PATH',
        requestId: 'fixture-request',
        retryable: false,
      },
    }
  }

  const items = listTemplateReleases(courseId).filter((release) =>
    actorResult.courseIds.includes(release.courseId),
  )
  const page = {
    items: items as EnvironmentTemplateReleaseViewSchema[],
    nextCursor: null,
  }
  return { status: 200, data: page }
}
