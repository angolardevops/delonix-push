export function ago(iso: string | null, now = Date.now()): string {
  if (!iso) return 'nunca'
  const s = Math.max(0, Math.round((now - new Date(iso).getTime()) / 1000))
  if (s < 60) return `há ${s} s`
  if (s < 3600) return `há ${Math.round(s / 60)} min`
  if (s < 86400) return `há ${Math.round(s / 3600)} h`
  return `há ${Math.round(s / 86400)} d`
}

export function secs(v: number | null): string {
  if (v === null) return '—'
  return v < 1 ? `${Math.round(v * 1000)} ms` : `${v.toFixed(1)} s`
}

export const stateLabel: Record<string, string> = {
  queued: 'na fila', sent: 'enviada', delivered: 'entregue', accepted: 'aceite pelo fornecedor', expired: 'expirada', failed: 'falhou',
}
