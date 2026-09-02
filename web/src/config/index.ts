/**
 * LabWeaver 前端运行时配置
 * 所有以 VITE_ 开头的变量在构建时会被 Vite 注入
 */

export const APP_TITLE = import.meta.env.VITE_APP_TITLE || 'LabWeaver'
// Generated OpenAPI paths already include /api/v1. This value is an origin/base only.
export const API_BASE_URL = import.meta.env.VITE_API_BASE_URL || '/'
const rawApiAuthMode = import.meta.env.VITE_API_AUTH_MODE || 'bff'
if (rawApiAuthMode !== 'bff' && rawApiAuthMode !== 'bearer') {
  throw new Error('VITE_API_AUTH_MODE must be either bff or bearer')
}
export const API_AUTH_MODE: 'bff' | 'bearer' = rawApiAuthMode

export const OIDC_CONFIG = {
  authority: import.meta.env.VITE_OIDC_AUTHORITY || '',
  client_id: import.meta.env.VITE_OIDC_CLIENT_ID || '',
  redirect_uri: import.meta.env.VITE_OIDC_REDIRECT_URI || window.location.origin + '/auth/callback',
  post_logout_redirect_uri: import.meta.env.VITE_OIDC_POST_LOGOUT_REDIRECT_URI || window.location.origin + '/',
  response_type: 'code',
  scope: 'openid profile email',
}

export const DIRECT_OIDC_ENABLED = Boolean(OIDC_CONFIG.authority && OIDC_CONFIG.client_id)
// The Sprint 2 browser uses the Access Service BFF: issuer and client identity
// are server-owned and therefore must not be duplicated into the Web image.
export const OIDC_ENABLED = API_AUTH_MODE === 'bff' || DIRECT_OIDC_ENABLED
