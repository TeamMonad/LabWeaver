<template>
  <DialogRoot v-if="isModal" :open="open" @update:open="(next) => !next && emit('close')">
    <DialogOverlay class="drawer-scrim" />
    <DialogContent
      as="aside"
      class="navigation-drawer navigation-drawer--open navigation-drawer--modal"
      :aria-describedby="undefined"
    >
      <VisuallyHidden>
        <DialogTitle>应用导航</DialogTitle>
      </VisuallyHidden>
      <NavigationDrawerBody is-modal :is-rail="false" @close="emit('close')" />
    </DialogContent>
  </DialogRoot>
  <aside
    v-else
    class="navigation-drawer navigation-drawer--open"
    :class="{ 'navigation-drawer--rail': isRail }"
    aria-label="应用导航"
  >
    <NavigationDrawerBody :is-modal="false" :is-rail="isRail" @toggle-rail="emit('toggleRail')" />
  </aside>
</template>

<script setup lang="ts">
import { computed, onMounted, onScopeDispose, ref } from 'vue'
import { DialogContent, DialogOverlay, DialogRoot, DialogTitle, VisuallyHidden } from 'reka-ui'
import NavigationDrawerBody from './NavigationDrawerBody.vue'

const props = defineProps<{
  open: boolean
  rail?: boolean
}>()

const emit = defineEmits<{
  close: []
  toggleRail: []
}>()

// Below 840px the drawer is a modal Reka Dialog: focus trap, Escape/scrim
// dismissal, scroll lock and focus return come from the primitive. At wider
// viewports it is a docked landmark that is always visible.
const viewportWidth = ref(typeof window === 'undefined' ? 1280 : window.innerWidth)
const isModal = computed(() => viewportWidth.value < 840)
const isRail = computed(() => !isModal.value && Boolean(props.rail))

function updateViewportWidth() {
  viewportWidth.value = window.innerWidth
}

onMounted(() => window.addEventListener('resize', updateViewportWidth))
onScopeDispose(() => window.removeEventListener('resize', updateViewportWidth))
</script>

<style scoped>
.navigation-drawer {
  position: fixed;
  top: 0;
  left: 0;
  z-index: 1200;
  display: flex;
  flex-direction: column;
  width: var(--nav-drawer-expanded);
  height: 100%;
  background: var(--md-sys-color-surface);
  border-right: 1px solid var(--md-sys-color-outline-variant);
  transform: translateX(-100%);
  transition: transform var(--motion-duration-slow) var(--motion-ease-drawer), width var(--motion-duration-slow) var(--motion-ease-drawer);
}

.navigation-drawer--open {
  transform: translateX(0);
}

.navigation-drawer--modal {
  outline: none;
}

.navigation-drawer--modal[data-state='open'] {
  animation: drawer-slide-in 0.25s ease;
}

.navigation-drawer--modal[data-state='closed'] {
  animation: drawer-slide-out 0.25s ease;
}

.drawer-scrim[data-state='closed'] {
  animation: drawer-fade-out 0.25s ease;
}

@keyframes drawer-slide-in {
  from { transform: translateX(-100%); }
  to { transform: translateX(0); }
}

@keyframes drawer-slide-out {
  from { transform: translateX(0); }
  to { transform: translateX(-100%); }
}

@keyframes drawer-fade-out {
  from { opacity: 1; }
  to { opacity: 0; }
}

.navigation-drawer--rail {
  width: var(--nav-drawer-collapsed);
}

.drawer-scrim {
  position: fixed;
  inset: 0;
  z-index: 1150;
  background: var(--md-sys-color-scrim);
}

@media (min-width: 840px) {
  .navigation-drawer {
    position: static;
    top: auto;
    left: auto;
    /* A grid item honours z-index; the docked drawer must stay below the top
       bar so the project selector and search menus are not covered. */
    z-index: auto;
    grid-row: 3;
    grid-column: 1;
    transform: none;
  }

  .drawer-scrim {
    display: none;
  }
}

@media (prefers-reduced-motion: reduce) {
  .navigation-drawer {
    transition: none;
  }

  .navigation-drawer--modal,
  .drawer-scrim {
    animation: none !important;
  }
}
</style>
