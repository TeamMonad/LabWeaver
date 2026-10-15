<template>
  <AppShell>
    <RouterView />
  </AppShell>
</template>

<script setup lang="ts">
import { onMounted } from 'vue'
import AppShell from '@/components/layout/AppShell.vue'
import { useAuth } from '@/composables/useAuth'

const auth = useAuth()

onMounted(() => {
  // Attempt to restore an existing OIDC session on app startup.
  auth.loadUser()
})
</script>

<style src="./styles/index.css"></style>
