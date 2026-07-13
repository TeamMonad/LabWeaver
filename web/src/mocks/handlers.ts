import { http, HttpResponse } from 'msw'

export const handlers = [
  http.get('/api/v1/health/live', () => {
    return HttpResponse.json({ status: 'ok' })
  }),

  http.get('/api/v1/health/ready', () => {
    return HttpResponse.json({ status: 'ok', checks: [] })
  }),

  http.get('/api/v1/me', () => {
    return HttpResponse.json({
      id: 'mock-user-001',
      name: 'Mock User',
      email: 'mock@labweaver.local',
      roles: ['student'],
    })
  }),

  http.get('/api/v1/courses', () => {
    return HttpResponse.json([
      { id: 'course-001', name: '算法实验', role: 'student' },
      { id: 'course-002', name: 'Linux 系统实验', role: 'student' },
    ])
  }),

  http.get('/api/v1/environments', () => {
    return HttpResponse.json([
      { id: 'env-001', name: 'OJ 实验环境', status: 'Ready', runtime: 'Container' },
      { id: 'env-002', name: 'Linux VM 实验', status: 'Stopped', runtime: 'VirtualMachine' },
    ])
  }),
]
