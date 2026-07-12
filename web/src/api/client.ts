import axios, { type AxiosInstance, type AxiosRequestConfig } from 'axios'
import { API_BASE_URL } from '@/config'

/**
 * LabWeaver API 客户端
 * 开发环境下通过 Vite proxy 转发到后端网关
 */
export const apiClient: AxiosInstance = axios.create({
  baseURL: API_BASE_URL,
  headers: {
    'Content-Type': 'application/json',
  },
  timeout: 30000,
})

apiClient.interceptors.request.use((config) => {
  const token = localStorage.getItem('access_token')
  if (token && config.headers) {
    config.headers.Authorization = `Bearer ${token}`
  }
  return config
})

apiClient.interceptors.response.use(
  (response) => response,
  (error) => {
    if (error.response?.status === 401) {
      // 由 OIDC/auth 模块统一处理登出/刷新
      console.warn('[apiClient] 401 Unauthorized')
    }
    return Promise.reject(error)
  },
)

export async function healthCheck(config?: AxiosRequestConfig) {
  return apiClient.get('/health/live', config)
}
