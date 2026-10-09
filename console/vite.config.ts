import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Em desenvolvimento a API corre noutra porta (`PUSH_API`, 8480 por omissão); em produção é o mesmo servidor.
const api = process.env.PUSH_API ?? 'http://127.0.0.1:8480'
export default defineConfig({
  plugins: [react()],
  server: { proxy: { '/console': api, '/v1': api, '/healthz': api } },
  test: { environment: 'node' },
})
