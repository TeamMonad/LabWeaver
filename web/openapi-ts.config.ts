import { defineConfig } from '@hey-api/openapi-ts'

export default defineConfig({
  input: '../schemas/openapi/labweaver-public.v1.json',
  output: 'src/generated/contracts',
  plugins: ['@hey-api/client-axios'],
})
