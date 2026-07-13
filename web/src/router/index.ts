import { createRouter, createWebHistory } from 'vue-router'
import type { RouteRecordRaw } from 'vue-router'

const routes: RouteRecordRaw[] = [
  {
    path: '/',
    name: 'home',
    component: () => import('@/views/HomeView.vue'),
  },
  {
    path: '/teacher',
    name: 'teacher',
    component: () => import('@/views/TeacherView.vue'),
    redirect: '/teacher/labs',
    children: [
      { path: 'labs', component: () => import('@/views/teacher/LabListView.vue') },
      { path: 'materials', component: () => import('@/views/teacher/MaterialUploadView.vue') },
      { path: 'approvals', component: () => import('@/views/teacher/ApprovalListView.vue') },
    ],
  },
  {
    path: '/student',
    name: 'student',
    component: () => import('@/views/StudentView.vue'),
    redirect: '/student/labs',
    children: [
      { path: 'labs', component: () => import('@/views/student/MyLabsView.vue') },
      { path: 'environments', component: () => import('@/views/student/EnvironmentEntryView.vue') },
      { path: 'results', component: () => import('@/views/student/ResultListView.vue') },
    ],
  },
  {
    path: '/researcher',
    name: 'researcher',
    component: () => import('@/views/ResearcherView.vue'),
    redirect: '/researcher/workspaces',
    children: [
      { path: 'workspaces', component: () => import('@/views/researcher/WorkspaceListView.vue') },
      { path: 'resources', component: () => import('@/views/researcher/ResourceRequestView.vue') },
      { path: 'config', component: () => import('@/views/researcher/SoftwareConfigView.vue') },
    ],
  },
  {
    path: '/admin',
    name: 'admin',
    component: () => import('@/views/AdminView.vue'),
    redirect: '/admin/resources',
    children: [
      { path: 'resources', component: () => import('@/views/admin/ResourceApprovalView.vue') },
      { path: 'policies', component: () => import('@/views/admin/PolicyListView.vue') },
      { path: 'audit', component: () => import('@/views/admin/AuditLogView.vue') },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    name: 'not-found',
    component: () => import('@/views/NotFoundView.vue'),
  },
]

const router = createRouter({
  history: createWebHistory(import.meta.env.BASE_URL),
  routes,
})

export default router
