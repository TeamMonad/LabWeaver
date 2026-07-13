export const REQUIREMENTS_BASELINE = Object.freeze({
  type: 'merged_pull_request',
  pr: 36,
  head: 'a9bc7a8ab013a35a846a4b428bad22ecc48eca1b',
  merge_commit: '0f80e4e9c4b2334d4a833d1fb6a2263ecc3dda9a',
})

export const ROLE_PROJECTS = Object.freeze([
  Object.freeze({
    name: 'setup',
    actor: 'technical authentication preparation',
    aliases: [],
    testMatch: /setup\/.*\.setup\.mjs$/,
    storageState: null,
  }),
  Object.freeze({
    name: 'teacher',
    actor: 'Teacher',
    aliases: ['teacher'],
    testMatch: /teacher\/.*\.spec\.mjs$/,
    storageState: '.auth/teacher.json',
  }),
  Object.freeze({
    name: 'student',
    actor: 'Student',
    aliases: ['student', 'researcher'],
    testMatch: /(?:student|researcher)\/.*\.spec\.mjs$/,
    storageState: '.auth/student.json',
  }),
  Object.freeze({
    name: 'platform-admin',
    actor: 'Platform Administrator',
    aliases: ['admin'],
    testMatch: /platform-admin\/.*\.spec\.mjs$/,
    storageState: '.auth/platform-admin.json',
  }),
])

export const PROJECT_NAMES = Object.freeze(ROLE_PROJECTS.map((project) => project.name))

export const ROLE_PROJECTS_BY_NAME = Object.freeze(
  Object.fromEntries(ROLE_PROJECTS.map((project) => [project.name, project])),
)
