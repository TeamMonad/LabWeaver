import '@/App.vue'
import '@fontsource-variable/roboto-flex'
import '@fontsource-variable/noto-sans-sc'
import '@fontsource/roboto/400.css'
import '@fontsource/roboto/500.css'
import 'material-symbols/rounded.css'
import { createApp } from 'vue'
import { createPinia } from 'pinia'
import FixturePreview from './FixturePreview.vue'
import router from './router'
import { initializeFixturePath, initializeFixtureQuery, initializeFixtureScene } from './scenes'

async function bootstrap() {
  const initialScene = initializeFixtureScene()
  await router.replace({
    path: initializeFixturePath(initialScene.path),
    query: initializeFixtureQuery(initialScene),
  })

  const app = createApp(FixturePreview)
  app.use(createPinia())
  app.use(router)
  await router.isReady()
  app.mount('#app')
}

void bootstrap()
