<template>
  <EnvironmentEntryView
    v-if="selectedEnvironmentId"
    teacher-mode
  />
  <MyLabsView
    v-else
    teacher-mode
    environment-path="/teacher/environments"
  />
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import EnvironmentEntryView from '@/views/student/EnvironmentEntryView.vue'
import MyLabsView from '@/views/student/MyLabsView.vue'

const route = useRoute()
const selectedEnvironmentId = computed(() => {
  const value = route.query.environmentId
  const id = Array.isArray(value) ? value[0] : value
  return typeof id === 'string' && id.trim() ? id.trim() : undefined
})
</script>
