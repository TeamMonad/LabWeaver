<template>
  <div id="app">
    <AppHeader />
    <main class="main-content">
      <RouterView />
    </main>
  </div>
</template>

<script setup lang="ts">
import AppHeader from '@/components/layout/AppHeader.vue'
</script>

<style>
/* Material You + GCP Console design tokens */
:root {
  /* Primary palette: GCP blue family */
  --md-sys-color-primary: #1a73e8;
  --md-sys-color-on-primary: #ffffff;
  --md-sys-color-primary-container: #d3e3fd;
  --md-sys-color-on-primary-container: #041e49;

  /* Secondary / Surface variants */
  --md-sys-color-secondary: #5f6368;
  --md-sys-color-on-secondary: #ffffff;
  --md-sys-color-secondary-container: #e8eaed;
  --md-sys-color-on-secondary-container: #202124;

  --md-sys-color-surface: #ffffff;
  --md-sys-color-surface-variant: #f1f3f4;
  --md-sys-color-background: #f8f9fa;
  --md-sys-color-on-surface: #202124;
  --md-sys-color-on-surface-variant: #5f6368;
  --md-sys-color-outline: #dadce0;
  --md-sys-color-outline-variant: #e8eaed;

  --md-sys-color-error: #d93025;
  --md-sys-color-on-error: #ffffff;

  /* Elevation shadows (Material You) */
  --md-sys-elevation-1: 0 1px 2px 0 rgba(60, 64, 67, 0.3), 0 1px 3px 1px rgba(60, 64, 67, 0.15);
  --md-sys-elevation-2: 0 1px 2px 0 rgba(60, 64, 67, 0.3), 0 2px 6px 2px rgba(60, 64, 67, 0.15);
  --md-sys-elevation-3: 0 1px 3px 0 rgba(60, 64, 67, 0.3), 0 4px 8px 3px rgba(60, 64, 67, 0.15);

  /* Shape */
  --md-sys-shape-small: 4px;
  --md-sys-shape-medium: 8px;
  --md-sys-shape-large: 12px;
  --md-sys-shape-extra-large: 16px;
  --md-sys-shape-full: 9999px;

  /* Typography scale */
  --md-sys-font-family: 'Google Sans', 'Roboto', 'Noto Sans SC', -apple-system, BlinkMacSystemFont,
    'Segoe UI', 'Helvetica Neue', Arial, sans-serif;
  --md-sys-headline-large: 600 32px/40px var(--md-sys-font-family);
  --md-sys-headline-medium: 600 28px/36px var(--md-sys-font-family);
  --md-sys-headline-small: 500 24px/32px var(--md-sys-font-family);
  --md-sys-title-large: 500 22px/28px var(--md-sys-font-family);
  --md-sys-title-medium: 500 16px/24px var(--md-sys-font-family);
  --md-sys-title-small: 500 14px/20px var(--md-sys-font-family);
  --md-sys-body-large: 400 16px/24px var(--md-sys-font-family);
  --md-sys-body-medium: 400 14px/20px var(--md-sys-font-family);
  --md-sys-body-small: 400 12px/16px var(--md-sys-font-family);
  --md-sys-label-large: 500 14px/20px var(--md-sys-font-family);
  --md-sys-label-medium: 500 12px/16px var(--md-sys-font-family);

  /* Layout */
  --app-header-height: 64px;
  --content-max-width: 1280px;
}

* {
  box-sizing: border-box;
  margin: 0;
  padding: 0;
}

body {
  font: var(--md-sys-body-medium);
  background: var(--md-sys-color-background);
  color: var(--md-sys-color-on-surface);
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
}

a {
  color: var(--md-sys-color-primary);
  text-decoration: none;
}

.main-content {
  max-width: var(--content-max-width);
  margin: 0 auto;
  padding: 24px 32px 48px;
  min-height: calc(100vh - var(--app-header-height));
}

/* Utility: Material You elevated card */
.md-card {
  background: var(--md-sys-color-surface);
  border-radius: var(--md-sys-shape-large);
  box-shadow: var(--md-sys-elevation-1);
  border: 1px solid var(--md-sys-color-outline-variant);
}

.md-card-filled {
  background: var(--md-sys-color-surface-variant);
  border-radius: var(--md-sys-shape-large);
}
</style>
