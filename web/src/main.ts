import { createApp } from 'vue'
import { createPinia } from 'pinia'
import router from './router'
import App from './App.vue'

// Self-hosted fonts (Issue #74: no third-party CDN in production)
import '@fontsource-variable/roboto-flex'
import '@fontsource-variable/noto-sans-sc'
import '@fontsource/roboto/400.css'
import '@fontsource/roboto/500.css'
import 'material-symbols/rounded.css'

const app = createApp(App)
app.use(createPinia())
app.use(router)
app.mount('#app')
