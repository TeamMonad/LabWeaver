<template>
  <span
    :class="[
      'material-symbols-rounded',
      'svg-icon',
      `svg-icon--${size}`,
      { 'material-symbols-rounded--filled': filled },
    ]"
    :aria-hidden="ariaLabel ? undefined : 'true'"
    :aria-label="ariaLabel"
    role="img"
  >
    {{ name }}
  </span>
</template>

<script setup lang="ts">
interface Props {
  name: string
  size?: 'sm' | 'md' | 'lg' | 'xl'
  filled?: boolean
  ariaLabel?: string
}

withDefaults(defineProps<Props>(), {
  size: 'md',
  filled: false,
})
</script>

<style scoped>
.svg-icon {
  display: inline-flex;
  flex-shrink: 0;
  align-items: center;
  justify-content: center;
  width: var(--svg-icon-size, 24px);
  height: var(--svg-icon-size, 24px);
  font-size: var(--svg-icon-size, 24px);
  line-height: 1;
  font-variation-settings: 'FILL' 0, 'wght' 400, 'GRAD' 0, 'opsz' 24;
}

.svg-icon--filled {
  font-variation-settings: 'FILL' 1, 'wght' 400, 'GRAD' 0, 'opsz' 24;
}

.svg-icon--sm {
  --svg-icon-size: 16px;
}

.svg-icon--md {
  --svg-icon-size: 20px;
}

.svg-icon--lg {
  --svg-icon-size: 24px;
}

.svg-icon--xl {
  --svg-icon-size: 32px;
}
</style>
