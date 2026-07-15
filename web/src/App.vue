<template>
  <AppShell>
    <RouterView />
  </AppShell>
</template>

<script setup lang="ts">
import AppShell from '@/components/layout/AppShell.vue'
</script>

<style>
/*
 * Material Design You frozen tokens for Sprint 2.
 * Source of truth: Issue #74 visual baseline v1 (approved 2026-07-14).
 * Do not add unregistered colors, shadows, radii or spacing values.
 */

:root {
  /* Core colors - light theme (default) */
  --md-sys-color-primary: #006A6A;
  --md-sys-color-on-primary: #FFFFFF;
  --md-sys-color-primary-container: #9CF1F0;
  --md-sys-color-on-primary-container: #002020;

  --md-sys-color-secondary: #4A6363;
  --md-sys-color-on-secondary: #FFFFFF;
  --md-sys-color-secondary-container: #CCE8E7;
  --md-sys-color-on-secondary-container: #051F1F;

  --md-sys-color-tertiary: #4B607C;
  --md-sys-color-on-tertiary: #FFFFFF;
  --md-sys-color-tertiary-container: #D3E4FF;
  --md-sys-color-on-tertiary-container: #041C35;

  --md-sys-color-surface: #F4FBFA;
  --md-sys-color-surface-container: #E8EFEE;
  --md-sys-color-surface-container-high: #E2E9E8;
  --md-sys-color-surface-variant: #DAE4E3;
  --md-sys-color-background: #F4FBFA;
  --md-sys-color-on-surface: #161D1D;
  --md-sys-color-on-surface-variant: #3F4948;
  --md-sys-color-outline: #6F7979;
  --md-sys-color-outline-variant: #BEC9C8;

  --md-sys-color-error: #BA1A1A;
  --md-sys-color-on-error: #FFFFFF;
  --md-sys-color-error-container: #FFDAD6;
  --md-sys-color-on-error-container: #410002;

  --md-sys-color-warning: #8A4F00;
  --md-sys-color-warning-container: #FFDDB6;
  --md-sys-color-success: #176B3A;
  --md-sys-color-success-container: #A6F5BC;

  --md-sys-color-inverse-surface: #2D3232;
  --md-sys-color-inverse-on-surface: #EFF5F4;
  --md-sys-color-scrim: rgba(0, 0, 0, 0.32);

  /* Elevation shadows */
  --md-sys-elevation-1: 0 1px 2px 0 rgba(0, 0, 0, 0.3), 0 1px 3px 1px rgba(0, 0, 0, 0.15);
  --md-sys-elevation-2: 0 1px 2px 0 rgba(0, 0, 0, 0.3), 0 2px 6px 2px rgba(0, 0, 0, 0.15);
  --md-sys-elevation-3: 0 1px 3px 0 rgba(0, 0, 0, 0.3), 0 4px 8px 3px rgba(0, 0, 0, 0.15);

  /* Shape */
  --md-sys-shape-small: 4px;
  --md-sys-shape-medium: 8px;
  --md-sys-shape-large: 12px;
  --md-sys-shape-extra-large: 16px;
  --md-sys-shape-full: 999px;

  /* Typography */
  --md-sys-font-family: 'Roboto Flex', 'Noto Sans SC', 'Roboto', system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', 'Helvetica Neue', Arial, sans-serif;
  --md-sys-headline-large: 400 32px/40px var(--md-sys-font-family);
  --md-sys-headline-medium: 400 28px/36px var(--md-sys-font-family);
  --md-sys-headline-small: 400 24px/32px var(--md-sys-font-family);
  --md-sys-title-large: 400 22px/28px var(--md-sys-font-family);
  --md-sys-title-medium: 500 16px/24px var(--md-sys-font-family);
  --md-sys-title-small: 500 14px/20px var(--md-sys-font-family);
  --md-sys-body-large: 400 16px/24px var(--md-sys-font-family);
  --md-sys-body-medium: 400 14px/20px var(--md-sys-font-family);
  --md-sys-body-small: 400 12px/16px var(--md-sys-font-family);
  --md-sys-label-large: 500 14px/20px var(--md-sys-font-family);
  --md-sys-label-medium: 500 12px/16px var(--md-sys-font-family);
  --md-sys-label-small: 500 11px/16px var(--md-sys-font-family);

  /* Layout */
  --app-top-bar-height: 64px;
  --nav-drawer-expanded: 256px;
  --nav-drawer-collapsed: 72px;
  --evidence-side-sheet-width: 360px;
  --content-max-width: 1440px;
}

[data-theme='dark'] {
  --md-sys-color-primary: #80D5D4;
  --md-sys-color-on-primary: #003737;
  --md-sys-color-primary-container: #004F4F;
  --md-sys-color-on-primary-container: #9CF1F0;

  --md-sys-color-secondary: #B0CCCB;
  --md-sys-color-on-secondary: #1C3534;
  --md-sys-color-secondary-container: #334B4B;
  --md-sys-color-on-secondary-container: #CCE8E7;

  --md-sys-color-tertiary: #B3C8E8;
  --md-sys-color-on-tertiary: #1B324A;
  --md-sys-color-tertiary-container: #334863;
  --md-sys-color-on-tertiary-container: #D3E4FF;

  --md-sys-color-surface: #0E1515;
  --md-sys-color-surface-container: #1A2121;
  --md-sys-color-surface-container-high: #252B2B;
  --md-sys-color-surface-variant: #3F4948;
  --md-sys-color-background: #0E1515;
  --md-sys-color-on-surface: #DEE4E3;
  --md-sys-color-on-surface-variant: #BEC9C8;
  --md-sys-color-outline: #899392;
  --md-sys-color-outline-variant: #3F4948;

  --md-sys-color-error: #FFB4AB;
  --md-sys-color-on-error: #690005;
  --md-sys-color-error-container: #93000A;
  --md-sys-color-on-error-container: #FFDAD6;

  --md-sys-color-warning: #FFB95F;
  --md-sys-color-warning-container: #562E00;
  --md-sys-color-success: #8FD8A9;
  --md-sys-color-success-container: #005227;

  --md-sys-color-inverse-surface: #DEE4E3;
  --md-sys-color-inverse-on-surface: #2D3232;
  --md-sys-color-scrim: rgba(0, 0, 0, 0.48);
}

* {
  box-sizing: border-box;
  margin: 0;
  padding: 0;
}

html,
body,
#app {
  height: 100%;
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

button,
a,
input,
select,
textarea,
[tabindex]:not([tabindex='-1']) {
  outline: none;
}

button:focus-visible,
a:focus-visible,
input:focus-visible,
select:focus-visible,
textarea:focus-visible,
[tabindex]:not([tabindex='-1']):focus-visible {
  outline: 2px solid var(--md-sys-color-primary);
  outline-offset: 2px;
}

@media (prefers-reduced-motion: reduce) {
  *,
  *::before,
  *::after {
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
    transition-duration: 0.01ms !important;
    scroll-behavior: auto !important;
  }
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
