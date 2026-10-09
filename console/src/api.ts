// Cliente da API de gestão (`/console/v1`). A sessão (`dps_…`) vive só em memória + sessionStorage: fecha o separador, sai.

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message)
  }
}

let token: string | null = null
try {
  token = sessionStorage.getItem('dps')
} catch {
  /* sem armazenamento: a sessão dura até recarregar */
}

export const hasSession = () => token !== null

export function setSession(t: string | null) {
  token = t
  try {
    if (t) sessionStorage.setItem('dps', t)
    else sessionStorage.removeItem('dps')
  } catch {
    /* ignora */
  }
}

export async function call<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
  const r = await fetch(path, {
    method,
    headers: {
      ...(body !== undefined ? { 'content-type': 'application/json' } : {}),
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
    body: body !== undefined ? JSON.stringify(body) : undefined,
  })
  if (r.status === 401 && token) {
    setSession(null)
    window.dispatchEvent(new Event('dps-expired'))
  }
  if (!r.ok) {
    let msg = `HTTP ${r.status}`
    try {
      msg = ((await r.json()) as { error?: string }).error ?? msg
    } catch {
      /* corpo vazio */
    }
    throw new ApiError(r.status, msg)
  }
  return r.status === 204 ? (undefined as T) : ((await r.json()) as T)
}

export interface Org { id: string; name: string; role: string }
export interface Project { id: string; name: string }
export interface ProjectInfo { id: string; org_id: string; name: string; devices: number; connected: number; limits: { rate_per_sec: number; daily_quota: number } }
export interface KeyRow { id: string; label: string; created_at: string; revoked_at: string | null }
export interface DeviceRow { id: string; platform: string; provider: string; created_at: string; last_seen_at: string | null; connected: boolean }
export interface MessageRow { id: string; device_id: string; state: string; priority: string; attempts: number; last_error: string | null; created_at: string }
export interface Stats { window_hours: number; by_state: Record<string, number>; hourly: { hour: string; messages: number }[]; delivery_latency_secs: { p50: number | null; p95: number | null } }
export interface CredRow { provider: string; meta: Record<string, unknown>; updated_at: string }

export const roleRank: Record<string, number> = { viewer: 0, developer: 1, admin: 2, owner: 3 }
export const can = (role: string, min: string) => (roleRank[role] ?? -1) >= roleRank[min]
