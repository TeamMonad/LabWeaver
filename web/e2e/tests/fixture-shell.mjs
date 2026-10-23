// Shell visual/a11y specs render the fixture preview (no backend). Each path is
// opened inside the preview's hash router with a fixed scene so the shell has
// an authenticated role and a settled project context.
export function fixtureUrl(path = '/', scene = 'home-teacher') {
  return `/fixture-preview.html#${path}?scene=${scene}`
}

// The teacher fixture role sees these task groups in the navigation drawer.
export const TEACHER_NAV_GROUPS = ['教学管理', '项目与工作']
