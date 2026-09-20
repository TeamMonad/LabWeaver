import { createRouter, createWebHashHistory, type RouteRecordRaw } from 'vue-router'
import FixtureRouteView from './FixtureRouteView.vue'
import { activeScene, fixtureProjectId, sceneById, setFixtureScene } from './scenes'

const paths = [
  '/',
  '/teacher/overview',
  '/teacher/labs',
  '/teacher/environments',
  '/teacher/materials',
  '/teacher/approvals',
  '/student/labs',
  '/student/environments',
  '/student/ssh-keys',
  '/student/results',
  '/researcher/workspaces',
  '/researcher/environments',
  '/researcher/software',
  '/researcher/resources',
  '/admin/resource-approval',
  '/admin/resource-finance',
  '/admin/platform-images',
  '/admin/policies',
  '/admin/audit',
]

const routes: RouteRecordRaw[] = paths.map((path, index) => ({
  path,
  name: `fixture-${index}`,
  component: FixtureRouteView,
}))

routes.push({ path: '/:pathMatch(.*)*', name: 'fixture-fallback', component: FixtureRouteView })

const router = createRouter({
  history: createWebHashHistory(import.meta.env.BASE_URL),
  routes,
  scrollBehavior: () => ({ left: 0, top: 0 }),
})

router.beforeEach((to) => {
  const requested = typeof to.query.scene === 'string' ? to.query.scene : undefined
  const scene = sceneById(requested ?? activeScene.value.id)
  setFixtureScene(scene.id)
  const query = {
    ...to.query,
    scene: scene.id,
  }
  if (to.query.scene !== scene.id) {
    return { path: to.path, query }
  }
  return undefined
})

export default router
export { fixtureProjectId }
