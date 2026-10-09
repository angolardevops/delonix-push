import { useCallback, useEffect, useState } from 'react'
import { call, hasSession, setSession, type Org, type Project } from './api'
import { Auth } from './Auth'
import { ProjectView } from './ProjectView'

export function App() {
  const [authed, setAuthed] = useState(hasSession())
  useEffect(() => {
    const out = () => setAuthed(false)
    window.addEventListener('dps-expired', out)
    return () => window.removeEventListener('dps-expired', out)
  }, [])
  if (!authed) return <Auth onDone={() => setAuthed(true)} />
  return <Shell onLogout={() => { void call('POST', '/console/v1/auth/logout').catch(() => {}); setSession(null); setAuthed(false) }} />
}

function Shell({ onLogout }: { onLogout: () => void }) {
  const [me, setMe] = useState('')
  const [orgs, setOrgs] = useState<Org[]>([])
  const [org, setOrg] = useState<Org | null>(null)
  const [projects, setProjects] = useState<Project[]>([])
  const [project, setProject] = useState<string | null>(null)
  const [err, setErr] = useState('')
  const [fresh, setFresh] = useState<{ name: string; key: string } | null>(null)
  const [creating, setCreating] = useState<'org' | 'project' | null>(null)
  const [newName, setNewName] = useState('')

  const loadOrgs = useCallback(async () => {
    const o = await call<Org[]>('GET', '/console/v1/orgs')
    setOrgs(o)
    setOrg((cur) => o.find((x) => x.id === cur?.id) ?? o[0] ?? null)
  }, [])
  useEffect(() => {
    call<{ email: string }>('GET', '/console/v1/me').then((m) => setMe(m.email)).catch(() => {})
    loadOrgs().catch((e) => setErr(String(e.message)))
  }, [loadOrgs])
  useEffect(() => {
    if (!org) { setProjects([]); return }
    call<Project[]>('GET', `/console/v1/orgs/${org.id}/projects`).then((p) => { setProjects(p); setProject((cur) => (p.some((x) => x.id === cur) ? cur : p[0]?.id ?? null)) }).catch((e) => setErr(String(e.message)))
  }, [org])

  async function createNamed(e: React.FormEvent) {
    e.preventDefault()
    const name = newName.trim()
    if (!name) return
    setErr('')
    try {
      if (creating === 'org') {
        await call('POST', '/console/v1/orgs', { name })
        await loadOrgs()
      } else if (org) {
        const p = await call<{ id: string; name: string; server_key: string }>('POST', `/console/v1/orgs/${org.id}/projects`, { name })
        setFresh({ name: p.name, key: p.server_key })
        const list = await call<Project[]>('GET', `/console/v1/orgs/${org.id}/projects`)
        setProjects(list)
        setProject(p.id)
      }
      setCreating(null)
      setNewName('')
    } catch (e) { setErr((e as Error).message) }
  }

  return (
    <div className="shell">
      <aside className="side">
        <div className="brand">delonix-push</div>
        <div>
          <label htmlFor="org">Organização</label>
          <select id="org" value={org?.id ?? ''} onChange={(e) => setOrg(orgs.find((o) => o.id === e.target.value) ?? null)}>
            {orgs.map((o) => <option key={o.id} value={o.id}>{o.name} ({o.role})</option>)}
          </select>
          <button className="btn ghost small" style={{ marginTop: 8 }} onClick={() => { setCreating('org'); setNewName('') }}>Nova organização</button>
        </div>
        <nav aria-label="Projectos">
          {projects.map((p) => <button key={p.id} aria-current={p.id === project} onClick={() => { setProject(p.id); setFresh(null) }}>{p.name}</button>)}
          {org && (org.role === 'owner' || org.role === 'admin') && <button className="muted" onClick={() => { setCreating('project'); setNewName('') }}>+ Novo projecto</button>}
        </nav>
        {creating && (
          <form onSubmit={createNamed} aria-label={creating === 'org' ? 'Nova organização' : 'Novo projecto'}>
            <label htmlFor="novo-nome">{creating === 'org' ? 'Nome da organização' : 'Nome do projecto (a tua app)'}</label>
            <input id="novo-nome" autoFocus maxLength={100} value={newName} onChange={(e) => setNewName(e.target.value)} />
            <div className="row" style={{ marginTop: 8 }}>
              <button className="btn" disabled={!newName.trim()}>Criar</button>
              <button type="button" className="btn ghost" onClick={() => setCreating(null)}>Cancelar</button>
            </div>
          </form>
        )}
        <div className="muted small" style={{ marginTop: 'auto' }}>
          {me}<br /><button className="btn ghost small" onClick={onLogout}>Sair</button>
        </div>
      </aside>
      <main className="main">
        {err && <div className="error" role="alert">{err}</div>}
        {fresh && (
          <div className="card">
            <h2>Projecto «{fresh.name}» criado</h2>
            <p className="small muted">Esta chave de servidor só se mostra agora. Guarda-a no teu servidor; nunca na app.</p>
            <div className="secret mono" data-testid="server-key">{fresh.key}</div>
          </div>
        )}
        {project && org ? <ProjectView key={project} id={project} role={org.role} /> : <p className="muted">Cria um projecto para começar.</p>}
      </main>
    </div>
  )
}
