import { useCallback, useEffect, useState } from 'react'
import { ago, secs, stateLabel } from './format'
import { call, can, type CredRow, type DeviceRow, type KeyRow, type MessageRow, type ProjectInfo, type Stats } from './api'

type Tab = 'geral' | 'chaves' | 'aparelhos' | 'mensagens' | 'teste' | 'credenciais'
const base = (id: string) => `/console/v1/projects/${id}`

function useLoad<T>(path: string | null, deps: unknown[] = []) {
  const [data, setData] = useState<T | null>(null)
  const [err, setErr] = useState('')
  const reload = useCallback(() => {
    if (!path) return
    call<T>('GET', path).then((d) => { setData(d); setErr('') }).catch((e) => setErr((e as Error).message))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path, ...deps])
  useEffect(reload, [reload])
  return { data, err, reload }
}

export function ProjectView({ id, role }: { id: string; role: string }) {
  const [tab, setTab] = useState<Tab>('geral')
  const info = useLoad<ProjectInfo>(base(id))
  const tabs: [Tab, string, boolean][] = [
    ['geral', 'Visão geral', true], ['aparelhos', 'Aparelhos', true], ['mensagens', 'Mensagens', true],
    ['teste', 'Enviar teste', can(role, 'developer')], ['chaves', 'Chaves', can(role, 'admin')], ['credenciais', 'Credenciais', can(role, 'admin')],
  ]
  return (
    <>
      <h1>{info.data?.name ?? '…'}</h1>
      <div className="muted small mono">{id}</div>
      {info.err && <div className="error" role="alert">{info.err}</div>}
      <div className="tabs" role="tablist">
        {tabs.filter(([, , show]) => show).map(([k, label]) => (
          <button key={k} role="tab" aria-selected={tab === k} onClick={() => setTab(k)}>{label}</button>
        ))}
      </div>
      {tab === 'geral' && <Overview id={id} info={info.data} />}
      {tab === 'aparelhos' && <Devices id={id} canRevoke={can(role, 'developer')} />}
      {tab === 'mensagens' && <Messages id={id} />}
      {tab === 'teste' && <SendTest id={id} />}
      {tab === 'chaves' && <Keys id={id} />}
      {tab === 'credenciais' && <Credentials id={id} />}
    </>
  )
}

function Overview({ id, info }: { id: string; info: ProjectInfo | null }) {
  const stats = useLoad<Stats>(`${base(id)}/stats`)
  const s = stats.data
  const total = s ? Object.values(s.by_state).reduce((a, b) => a + b, 0) : 0
  const max = Math.max(1, ...(s?.hourly.map((h) => h.messages) ?? [1]))
  return (
    <>
      <div className="grid">
        <div className="stat"><span className="muted small">Aparelhos</span><b>{info?.devices ?? '…'}</b></div>
        <div className="stat"><span className="muted small">Ligados agora</span><b>{info?.connected ?? '…'}</b></div>
        <div className="stat"><span className="muted small">Mensagens (24 h)</span><b>{total}</b></div>
        <div className="stat"><span className="muted small">Entregues</span><b>{s?.by_state.delivered ?? 0}</b></div>
        <div className="stat"><span className="muted small">Latência p50 / p95</span><b style={{ fontSize: 18 }}>{secs(s?.delivery_latency_secs.p50 ?? null)} / {secs(s?.delivery_latency_secs.p95 ?? null)}</b></div>
      </div>
      <div className="card" style={{ marginTop: 16 }}>
        <h2>Mensagens por hora (últimas 24 h)</h2>
        {s && s.hourly.length > 0 ? (
          <div className="bars" role="img" aria-label="Mensagens por hora">
            {s.hourly.map((h) => <i key={h.hour} title={`${new Date(h.hour).toLocaleTimeString()}: ${h.messages}`} style={{ height: `${(h.messages / max) * 100}%` }} />)}
          </div>
        ) : <p className="muted">Ainda sem mensagens nas últimas 24 h.</p>}
        {s && Object.keys(s.by_state).length > 0 && (
          <p className="small">{Object.entries(s.by_state).map(([k, v]) => <span key={k} className={`pill ${k}`} style={{ marginRight: 6 }}>{stateLabel[k] ?? k}: {v}</span>)}</p>
        )}
      </div>
      {info && <p className="muted small">Limites: {info.limits.rate_per_sec} mensagens/s · {info.limits.daily_quota.toLocaleString('pt-PT')} por dia (definidos pelo operador da plataforma).</p>}
    </>
  )
}

function Devices({ id, canRevoke }: { id: string; canRevoke: boolean }) {
  const { data, err, reload } = useLoad<DeviceRow[]>(`${base(id)}/devices`)
  async function revoke(d: string) {
    if (!confirm('Revogar este aparelho? Deixa de receber mensagens.')) return
    await call('DELETE', `${base(id)}/devices/${d}`).catch((e) => alert((e as Error).message))
    reload()
  }
  return (
    <div className="card">
      <div className="row"><h2 className="grow">Aparelhos</h2><button className="btn ghost" onClick={reload}>Actualizar</button></div>
      {err && <div className="error">{err}</div>}
      {data && data.length === 0 && <p className="muted">Nenhum aparelho. O servidor da tua app regista-os com <span className="mono">POST /v1/devices</span>.</p>}
      {data && data.length > 0 && (
        <table>
          <thead><tr><th>Aparelho</th><th>Plataforma</th><th>Via</th><th>Estado</th><th>Visto</th><th></th></tr></thead>
          <tbody>{data.map((d) => (
            <tr key={d.id}>
              <td className="mono">{d.id.slice(0, 8)}…</td><td>{d.platform}</td><td>{d.provider}</td>
              <td><span className={`pill ${d.connected ? 'on' : ''}`}>{d.connected ? 'ligado' : 'offline'}</span></td>
              <td>{ago(d.last_seen_at)}</td>
              <td>{canRevoke && <button className="btn ghost" onClick={() => revoke(d.id)}>Revogar</button>}</td>
            </tr>
          ))}</tbody>
        </table>
      )}
    </div>
  )
}

function Messages({ id }: { id: string }) {
  const [state, setState] = useState('')
  const { data, err, reload } = useLoad<MessageRow[]>(`${base(id)}/messages${state ? `?state=${state}` : ''}`)
  return (
    <div className="card">
      <div className="row">
        <h2 className="grow">Mensagens</h2>
        <select aria-label="Estado" value={state} onChange={(e) => setState(e.target.value)} style={{ width: 200 }}>
          <option value="">todos os estados</option>
          {Object.entries(stateLabel).map(([k, v]) => <option key={k} value={k}>{v}</option>)}
        </select>
        <button className="btn ghost" onClick={reload}>Actualizar</button>
      </div>
      <p className="muted small">O conteúdo das mensagens não se mostra aqui: é do remetente.</p>
      {err && <div className="error">{err}</div>}
      {data && data.length === 0 && <p className="muted">Sem mensagens.</p>}
      {data && data.length > 0 && (
        <table>
          <thead><tr><th>Mensagem</th><th>Aparelho</th><th>Estado</th><th>Tentativas</th><th>Quando</th><th>Erro</th></tr></thead>
          <tbody>{data.map((m) => (
            <tr key={m.id}>
              <td className="mono">{m.id.slice(0, 8)}…</td><td className="mono">{m.device_id.slice(0, 8)}…</td>
              <td><span className={`pill ${m.state}`}>{stateLabel[m.state] ?? m.state}</span></td>
              <td>{m.attempts}</td><td>{ago(m.created_at)}</td><td className="small">{m.last_error ?? ''}</td>
            </tr>
          ))}</tbody>
        </table>
      )}
    </div>
  )
}

function SendTest({ id }: { id: string }) {
  const devices = useLoad<DeviceRow[]>(`${base(id)}/devices`)
  const [target, setTarget] = useState('')
  const [topic, setTopic] = useState('')
  const [payload, setPayload] = useState('{\n  "ola": "mundo"\n}')
  const [priority, setPriority] = useState('normal')
  const [res, setRes] = useState('')
  const [err, setErr] = useState('')
  async function send(e: React.FormEvent) {
    e.preventDefault(); setErr(''); setRes('')
    let parsed: unknown
    try { parsed = JSON.parse(payload) } catch { setErr('O conteúdo não é JSON válido.'); return }
    try {
      const r = await call<{ message_ids: string[] }>('POST', `${base(id)}/messages`, { ...(topic ? { topic } : { device_id: target }), payload: parsed, priority })
      setRes(`Aceite: ${r.message_ids.length} mensagem(ns).`)
    } catch (e) { setErr((e as Error).message) }
  }
  return (
    <form className="card" onSubmit={send}>
      <h2>Enviar mensagem de teste</h2>
      <label htmlFor="alvo">Aparelho</label>
      <select id="alvo" value={target} onChange={(e) => setTarget(e.target.value)} disabled={!!topic}>
        <option value="">— escolhe —</option>
        {devices.data?.map((d) => <option key={d.id} value={d.id}>{d.id.slice(0, 8)}… · {d.platform}{d.connected ? ' · ligado' : ''}</option>)}
      </select>
      <label htmlFor="topico">…ou tópico (difusão)</label>
      <input id="topico" value={topic} onChange={(e) => setTopic(e.target.value)} placeholder="ex.: sala-1" />
      <label htmlFor="prio">Prioridade</label>
      <select id="prio" value={priority} onChange={(e) => setPriority(e.target.value)}><option value="normal">normal</option><option value="high">alta</option></select>
      <label htmlFor="payload">Conteúdo (JSON, até 4 KB)</label>
      <textarea id="payload" value={payload} onChange={(e) => setPayload(e.target.value)} spellCheck={false} />
      {err && <div className="error" role="alert">{err}</div>}{res && <div className="ok" role="status">{res}</div>}
      <button className="btn" disabled={!target && !topic}>Enviar</button>
    </form>
  )
}

function Keys({ id }: { id: string }) {
  const { data, err, reload } = useLoad<KeyRow[]>(`${base(id)}/keys`)
  const [label, setLabel] = useState('')
  const [fresh, setFresh] = useState('')
  async function create(e: React.FormEvent) {
    e.preventDefault()
    const r = await call<{ key: string }>('POST', `${base(id)}/keys`, { label }).catch((x) => { alert((x as Error).message); return null })
    if (r) { setFresh(r.key); setLabel(''); reload() }
  }
  async function revoke(k: string) {
    if (!confirm('Revogar esta chave? Os servidores que a usam deixam de poder enviar.')) return
    await call('DELETE', `${base(id)}/keys/${k}`).catch((x) => alert((x as Error).message))
    reload()
  }
  return (
    <>
      {fresh && <div className="card"><h2>Nova chave</h2><p className="small muted">Só se mostra agora.</p><div className="secret mono" data-testid="new-key">{fresh}</div></div>}
      <div className="card">
        <h2>Chaves de servidor</h2>
        {err && <div className="error">{err}</div>}
        <table>
          <thead><tr><th>Rótulo</th><th>Criada</th><th>Estado</th><th></th></tr></thead>
          <tbody>{data?.map((k) => (
            <tr key={k.id}><td>{k.label || <span className="muted">(sem rótulo)</span>}</td><td>{ago(k.created_at)}</td>
              <td><span className={`pill ${k.revoked_at ? 'failed' : 'on'}`}>{k.revoked_at ? 'revogada' : 'activa'}</span></td>
              <td>{!k.revoked_at && <button className="btn ghost" onClick={() => revoke(k.id)}>Revogar</button>}</td></tr>
          ))}</tbody>
        </table>
        <form className="row" style={{ marginTop: 12 }} onSubmit={create}>
          <input aria-label="Rótulo da nova chave" className="grow" placeholder="Rótulo (ex.: produção)" value={label} onChange={(e) => setLabel(e.target.value)} />
          <button className="btn">Criar chave</button>
        </form>
        <p className="muted small">Para rodar uma chave: cria a nova, actualiza o servidor e só depois revoga a antiga.</p>
      </div>
    </>
  )
}

function Credentials({ id }: { id: string }) {
  const { data, err, reload } = useLoad<CredRow[]>(`${base(id)}/credentials`)
  const [fcm, setFcm] = useState('')
  const [apns, setApns] = useState({ key_id: '', team_id: '', topic: '', p8: '', voip: false, sandbox: false })
  const [msg, setMsg] = useState('')
  const [bad, setBad] = useState('')
  async function save(kind: 'fcm' | 'apns') {
    setMsg(''); setBad('')
    try {
      let body: unknown
      if (kind === 'fcm') { try { body = { service_account: JSON.parse(fcm) } } catch { setBad('O ficheiro da conta de serviço não é JSON válido.'); return } }
      else body = apns
      await call('PUT', `${base(id)}/credentials/${kind}`, body)
      setMsg(`Credencial ${kind.toUpperCase()} guardada (cifrada).`); setFcm(''); setApns({ ...apns, p8: '' }); reload()
    } catch (e) { setBad((e as Error).message) }
  }
  async function remove(kind: string) {
    if (!confirm(`Remover a credencial ${kind}?`)) return
    await call('DELETE', `${base(id)}/credentials/${kind}`).catch((e) => setBad((e as Error).message)); reload()
  }
  const has = (k: string) => data?.find((c) => c.provider === k)
  return (
    <>
      <p className="muted small">As chaves são cifradas no servidor e nunca voltam a mostrar-se. Sem credenciais, o projecto só entrega pela ligação própria.</p>
      {err && <div className="error">{err}</div>}{msg && <div className="ok" role="status">{msg}</div>}{bad && <div className="error" role="alert">{bad}</div>}
      <div className="card">
        <h2>Android · FCM {has('fcm') && <span className="pill on">configurado</span>}</h2>
        {has('fcm') && <p className="small">Conta de serviço: <span className="mono">{String(has('fcm')!.meta.client_email)}</span> · {ago(has('fcm')!.updated_at)} <button className="btn ghost" onClick={() => remove('fcm')}>Remover</button></p>}
        <label htmlFor="fcm">JSON da conta de serviço do Firebase</label>
        <textarea id="fcm" value={fcm} onChange={(e) => setFcm(e.target.value)} spellCheck={false} placeholder='{"type":"service_account","project_id":"…","client_email":"…","private_key":"-----BEGIN PRIVATE KEY-----…"}' />
        <button className="btn" disabled={!fcm.trim()} onClick={() => save('fcm')}>Guardar FCM</button>
      </div>
      <div className="card">
        <h2>iPhone · APNs {has('apns') && <span className="pill on">configurado</span>}</h2>
        {has('apns') && <p className="small">Chave <span className="mono">{String(has('apns')!.meta.key_id)}</span> · tópico <span className="mono">{String(has('apns')!.meta.topic)}</span> · {ago(has('apns')!.updated_at)} <button className="btn ghost" onClick={() => remove('apns')}>Remover</button></p>}
        <div className="grid">
          <div><label htmlFor="kid">Key ID</label><input id="kid" value={apns.key_id} onChange={(e) => setApns({ ...apns, key_id: e.target.value })} /></div>
          <div><label htmlFor="tid">Team ID</label><input id="tid" value={apns.team_id} onChange={(e) => setApns({ ...apns, team_id: e.target.value })} /></div>
          <div><label htmlFor="top">Bundle ID (tópico)</label><input id="top" value={apns.topic} onChange={(e) => setApns({ ...apns, topic: e.target.value })} /></div>
        </div>
        <label htmlFor="p8">Chave .p8</label>
        <textarea id="p8" value={apns.p8} onChange={(e) => setApns({ ...apns, p8: e.target.value })} spellCheck={false} placeholder="-----BEGIN PRIVATE KEY-----" />
        <div className="row"><label><input type="checkbox" style={{ width: 'auto' }} checked={apns.voip} onChange={(e) => setApns({ ...apns, voip: e.target.checked })} /> VoIP (chamadas, CallKit)</label>
          <label><input type="checkbox" style={{ width: 'auto' }} checked={apns.sandbox} onChange={(e) => setApns({ ...apns, sandbox: e.target.checked })} /> Sandbox</label></div>
        <button className="btn" disabled={!apns.key_id || !apns.team_id || !apns.topic || !apns.p8.trim()} onClick={() => save('apns')}>Guardar APNs</button>
      </div>
    </>
  )
}
