<template>
  <component :is="pageComponent" :key="route.fullPath" />
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import HomeView from '@/views/HomeView.vue'
import TeacherOverviewView from '@/views/teacher/TeacherOverviewView.vue'
import LabListView from '@/views/teacher/LabListView.vue'
import EnvironmentEntryView from '@/views/student/EnvironmentEntryView.vue'
import ResourceRequestView from '@/views/researcher/ResourceRequestView.vue'
import WorkspaceListView from '@/views/researcher/WorkspaceListView.vue'
import ResourceApprovalView from '@/views/admin/ResourceApprovalView.vue'
import ResourceFinanceView from '@/views/admin/ResourceFinanceView.vue'
import PlatformImageView from '@/views/admin/PlatformImageView.vue'
import WorkbenchModuleView from '@/views/teacher/WorkbenchModuleView.vue'
import FixtureUnsupportedRoute from './FixtureUnsupportedRoute.vue'

const route = useRoute()

const routePage = computed(() => {
  if (route.path === '/') return HomeView
  if (route.path.startsWith('/teacher/overview')) return TeacherOverviewView
  if (route.path.startsWith('/teacher/labs')) return LabListView
  if (route.path.startsWith('/teacher/materials')) return FixtureUnsupportedRoute
  if (route.path.startsWith('/teacher/approvals')) return FixtureUnsupportedRoute
  if (route.path.startsWith('/student/environments')) return EnvironmentEntryView
  if (route.path.startsWith('/student/labs')) return FixtureUnsupportedRoute
  if (route.path.startsWith('/student/ssh-keys')) return FixtureUnsupportedRoute
  if (route.path.startsWith('/student/results')) return FixtureUnsupportedRoute
  if (route.path.startsWith('/researcher/resources')) return ResourceRequestView
  if (route.path.startsWith('/researcher/workspaces')) return WorkspaceListView
  if (route.path.startsWith('/researcher/environments')) return EnvironmentEntryView
  if (route.path.startsWith('/researcher/software')) return FixtureUnsupportedRoute
  if (route.path.startsWith('/admin/resource-approval')) return ResourceApprovalView
  if (route.path.startsWith('/admin/resource-finance')) return ResourceFinanceView
  if (route.path.startsWith('/admin/platform-images')) return PlatformImageView
  if (route.path.startsWith('/teacher/environments')) return WorkbenchModuleView
  if (route.path.startsWith('/admin/')) return FixtureUnsupportedRoute
  return FixtureUnsupportedRoute
})

const pageComponent = routePage
</script>
