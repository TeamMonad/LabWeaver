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
    aliases: ['student'],
    testMatch: /student\/.*\.spec\.mjs$/,
    storageState: '.auth/student.json',
  }),
  Object.freeze({
    name: 'platform-admin',
    actor: 'Platform Administrator',
    aliases: ['admin'],
    testMatch: /platform-admin\/.*\.spec\.mjs$/,
    storageState: '.auth/platform-admin.json',
  }),
  Object.freeze({
    name: 'visual-regression',
    actor: 'Visual regression',
    aliases: [],
    testMatch: /tests\/.*\.visual\.spec\.mjs$/,
    storageState: null,
  }),
  Object.freeze({
    name: 'a11y',
    actor: 'Accessibility scan',
    aliases: [],
    testMatch: /tests\/.*\.a11y\.spec\.mjs$/,
    storageState: null,
  }),
])

export const PROJECT_NAMES = Object.freeze(ROLE_PROJECTS.map((project) => project.name))

export const ROLE_PROJECTS_BY_NAME = Object.freeze(
  Object.fromEntries(ROLE_PROJECTS.map((project) => [project.name, project])),
)
