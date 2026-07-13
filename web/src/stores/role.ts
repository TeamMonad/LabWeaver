import { defineStore } from 'pinia'
import { ref, computed } from 'vue'

export type UserRole = 'teacher' | 'student' | 'researcher' | 'admin' | null

export const useRoleStore = defineStore('role', () => {
  const currentRole = ref<UserRole>(null)

  const isAuthenticated = computed(() => currentRole.value !== null)
  const roleLabel = computed(() => {
    switch (currentRole.value) {
      case 'teacher': return '教师'
      case 'student': return '学生'
      case 'researcher': return '科研用户'
      case 'admin': return '管理员'
      default: return '访客'
    }
  })

  function setRole(role: UserRole) {
    currentRole.value = role
  }

  function clearRole() {
    currentRole.value = null
  }

  return {
    currentRole,
    isAuthenticated,
    roleLabel,
    setRole,
    clearRole,
  }
})
