import type { FixtureRequest } from '../types'

export type FixtureRole = 'platform-admin' | 'teacher' | 'student'

export interface FixtureActor {
  actorId: string
  role: FixtureRole
  courseIds: string[]
}

const ROLE_COURSES: Record<FixtureRole, string[]> = {
  'platform-admin': ['course-admin'],
  teacher: ['course-101', 'course-102'],
  student: ['course-101'],
}

const VALID_ROLES: readonly string[] = Object.keys(ROLE_COURSES)

export function isValidRole(value: string): value is FixtureRole {
  return VALID_ROLES.includes(value)
}

export function parseActor(req: FixtureRequest): FixtureActor | null {
  const auth = req.headers.Authorization ?? req.headers.authorization ?? ''
  const match = /^Bearer fixture-(?<role>[a-z-]+)$/.exec(auth)
  if (!match?.groups?.role) return null
  const role = match.groups.role
  if (!isValidRole(role)) return null
  return {
    actorId: `fixture-actor-${role}`,
    role,
    courseIds: ROLE_COURSES[role],
  }
}

export type FixtureAction =
  | 'environment:read'
  | 'environment:write'
  | 'environment:delete'
  | 'access_grant:read'
  | 'access_grant:write'
  | 'access_grant:revoke'
  | 'llm_policy:read'
  | 'problem_package:write'
  | 'agent_run:read'
  | 'agent_run:write'
  | 'candidate:read'
  | 'candidate:approve'
  | 'release:read'
  | 'release:publish'
  | 'console_capability:read'
  | 'console_capability:issue'
  | 'ssh_key:read'
  | 'ssh_key:write'
  | 'events:read'
  | 'submission:freeze'
  | 'resource_request:read'
  | 'resource_request:write'
  | 'resource_request:approve'
  | 'resource_request:cancel'
  | 'resource_request:retry'
  | 'resource_lease:read'
  | 'resource_lease:renew'
  | 'resource_lease:revoke'

export interface FixtureResource {
  courseId?: string
  actorId?: string
}

export function can(role: FixtureRole, action: FixtureAction, resource?: FixtureResource): boolean {
  switch (role) {
    case 'platform-admin':
      return true
    case 'teacher': {
      switch (action) {
        case 'environment:read':
        case 'environment:write':
        case 'environment:delete':
        case 'access_grant:read':
        case 'access_grant:write':
        case 'access_grant:revoke':
        case 'llm_policy:read':
        case 'problem_package:write':
        case 'agent_run:read':
        case 'agent_run:write':
        case 'submission:freeze':
        case 'candidate:read':
        case 'candidate:approve':
        case 'release:read':
        case 'release:publish':
        case 'console_capability:read':
        case 'console_capability:issue':
          return resource?.courseId === undefined || ROLE_COURSES.teacher.includes(resource.courseId)
        case 'ssh_key:read':
        case 'ssh_key:write':
          return resource?.actorId === undefined || resource.actorId.startsWith('fixture-actor-teacher')
        case 'events:read':
          return true
        // 教师可以提交并读取本人的资源申请；审批、取消、重试与 Lease 管理仅 platform-admin。
        case 'resource_request:read':
        case 'resource_request:write':
          return true
        default:
          return false
      }
    }
    case 'student': {
      switch (action) {
        case 'environment:read':
        case 'environment:write':
        case 'access_grant:read':
        case 'access_grant:write':
        case 'access_grant:revoke':
        case 'events:read':
        case 'submission:freeze':
        case 'console_capability:read':
        case 'console_capability:issue':
          return resource?.courseId === undefined || ROLE_COURSES.student.includes(resource.courseId)
        case 'ssh_key:read':
        case 'ssh_key:write':
          return resource?.actorId === undefined || resource.actorId.startsWith('fixture-actor-student')
        // 学生可以提交并读取本人的资源申请；审批与 Lease 管理仅 platform-admin。
        case 'resource_request:read':
        case 'resource_request:write':
          return true
        default:
          return false
      }
    }
    default:
      return false
  }
}
