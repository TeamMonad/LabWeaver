<template>
  <section class="teacher-shell">
    <RouterView />
  </section>
</template>

<script setup lang="ts">
import { RouterView } from 'vue-router'
</script>

<style scoped>
.teacher-shell {
  min-width: 0;
}
</style>
