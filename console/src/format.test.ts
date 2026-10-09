import { describe, expect, it } from 'vitest'
import { ago, secs } from './format'
import { can } from './api'

describe('format', () => {
  it('ago', () => {
    const now = Date.parse('2026-10-09T12:00:00Z')
    expect(ago(null, now)).toBe('nunca')
    expect(ago('2026-10-09T11:59:30Z', now)).toBe('há 30 s')
    expect(ago('2026-10-09T11:00:00Z', now)).toBe('há 1 h')
    expect(ago('2026-10-07T12:00:00Z', now)).toBe('há 2 d')
  })
  it('secs', () => {
    expect(secs(null)).toBe('—')
    expect(secs(0.25)).toBe('250 ms')
    expect(secs(3.14)).toBe('3.1 s')
  })
  it('papéis', () => {
    expect(can('viewer', 'developer')).toBe(false)
    expect(can('owner', 'admin')).toBe(true)
    expect(can('desconhecido', 'viewer')).toBe(false)
  })
})
