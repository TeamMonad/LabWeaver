import { computed, ref } from 'vue'
import { activeScene } from './scenes'

const error = ref<Error | null>(null)
const isLoading = ref(false)

const user = computed(() => ({
  expired: false,
  profile: {
    actor_id: `fixture-${activeScene.value.role}`,
    roles: [activeScene.value.role],
    name: activeScene.value.role === 'teacher' ? '林老师（fixture）' : activeScene.value.role === 'student' ? '周同学（fixture）' : '平台管理员（fixture）',
    preferred_username: activeScene.value.role === 'teacher' ? 'teacher.demo' : activeScene.value.role === 'student' ? 'student.demo' : 'admin.demo',
    email: `${activeScene.value.role}@fixture.labweaver.local`,
    course_id: 'course-physics-2026',
  },
}))

const isAuthenticated = computed(() => true)

export function useAuth() {
  async function loadUser() {
    isLoading.value = false
  }

  async function login() {
    error.value = new Error('Fixture 预览不会执行真实登录。')
  }

  async function logout() {
    error.value = new Error('Fixture 预览不会执行真实退出。')
  }

  async function handleCallback() {
    isLoading.value = false
  }

  return { user, isLoading, error, isAuthenticated, login, logout, handleCallback, loadUser }
}

export async function getOidcAccessToken(): Promise<string | undefined> {
  return undefined
}
