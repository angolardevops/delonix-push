import { useState } from 'react'
import { call, setSession } from './api'

export function Auth({ onDone }: { onDone: () => void }) {
  const [mode, setMode] = useState<'login' | 'register'>('login')
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    setErr(''); setBusy(true)
    try {
      if (mode === 'register') await call('POST', '/console/v1/auth/register', { email, password })
      const r = await call<{ token: string }>('POST', '/console/v1/auth/login', { email, password })
      setSession(r.token)
      onDone()
    } catch (e) { setErr((e as Error).message) } finally { setBusy(false) }
  }

  return (
    <div className="auth">
      <h1>delonix-push</h1>
      <p className="muted">Notificações push para as tuas apps, com ligação própria e FCM/APNs.</p>
      <form className="card" onSubmit={submit}>
        <h2>{mode === 'login' ? 'Entrar' : 'Criar conta'}</h2>
        <label htmlFor="email">E-mail</label>
        <input id="email" type="email" autoComplete="username" required value={email} onChange={(e) => setEmail(e.target.value)} />
        <label htmlFor="pw">Senha {mode === 'register' && <span className="small">(mínimo 10 caracteres)</span>}</label>
        <input id="pw" type="password" autoComplete={mode === 'login' ? 'current-password' : 'new-password'} required minLength={mode === 'register' ? 10 : 1} value={password} onChange={(e) => setPassword(e.target.value)} />
        {err && <div className="error" role="alert">{err}</div>}
        <div className="row" style={{ marginTop: 16 }}>
          <button className="btn" disabled={busy}>{mode === 'login' ? 'Entrar' : 'Criar conta'}</button>
          <button type="button" className="btn ghost" onClick={() => { setMode(mode === 'login' ? 'register' : 'login'); setErr('') }}>
            {mode === 'login' ? 'Criar conta' : 'Já tenho conta'}
          </button>
        </div>
      </form>
    </div>
  )
}
