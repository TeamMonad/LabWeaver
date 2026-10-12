import { createRouter, createWebHashHistory, type RouteRecordRaw } from 'vue-router'
import FixtureRouteView from './FixtureRouteView.vue'
import { NAVIGATION_DETAIL_ROUTES, NAVIGATION_GROUPS } from '@/utils/navigation'
import { activeScene, fixtureProjectId, sceneById, setFixtureScene } from './scenes'

// Paths, titles and project scope come from the application navigation config
// so the preview cannot drift from the real router.
const routes: RouteRecordRaw[] = [
  { path: '/', name: 'fixture-home', component: FixtureRouteView, meta: { title: '进行中', projectScoped: true } },
  ...NAVIGATION_GROUPS.flatMap((group) => group.items.map((item) => ({
    path: item.path,
    name: `fixture-${item.id}`,
    component: FixtureRouteView,
    meta: { title: item.label, projectScoped: Boolean(item.projectScoped) },
  }))),
  ...NAVIGATION_DETAIL_ROUTES.map((detail) => ({
    path: detail.path,
    name: `fixture-detail-${detail.parentId}`,
    component: FixtureRouteView,
    meta: { title: detail.title, projectScoped: true },
  })),
]

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
