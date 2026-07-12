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
  },
  {
    path: '/student',
    name: 'student',
    component: () => import('@/views/StudentView.vue'),
  },
  {
    path: '/researcher',
    name: 'researcher',
    component: () => import('@/views/ResearcherView.vue'),
  },
  {
    path: '/admin',
    name: 'admin',
    component: () => import('@/views/AdminView.vue'),
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
