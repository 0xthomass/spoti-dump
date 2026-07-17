import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { apiRequest } from './client'

type ResponseInit = {
  ok?: boolean
  status?: number
  json?: () => Promise<unknown>
}

function makeResponse({ ok = true, status = 200, json }: ResponseInit) {
  return {
    ok,
    status,
    json: json ?? (() => Promise.resolve({})),
  } as unknown as Response
}

const fetchMock = vi.fn<typeof fetch>()

beforeEach(() => {
  vi.stubGlobal('fetch', fetchMock)
})

afterEach(() => {
  vi.unstubAllGlobals()
  fetchMock.mockReset()
})

describe('apiRequest', () => {
  it('returns the parsed body on a 2xx response and prefixes /api', async () => {
    fetchMock.mockResolvedValue(
      makeResponse({ json: () => Promise.resolve({ hello: 'world' }) }),
    )

    const result = await apiRequest<{ hello: string }>('/tracks/1')

    expect(result).toEqual({ hello: 'world' })
    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(fetchMock.mock.calls[0][0]).toBe('/api/tracks/1')
  })

  it('throws the server-provided error message on a non-ok response', async () => {
    fetchMock.mockResolvedValue(
      makeResponse({
        ok: false,
        status: 400,
        json: () => Promise.resolve({ error: 'Track is locked' }),
      }),
    )

    await expect(apiRequest('/tracks/1')).rejects.toThrow('Track is locked')
  })

  it('throws a status-based message when the error field is absent', async () => {
    fetchMock.mockResolvedValue(
      makeResponse({
        ok: false,
        status: 502,
        json: () => Promise.resolve({ detail: 'nope' }),
      }),
    )

    await expect(apiRequest('/tracks/1')).rejects.toThrow('Request failed (502)')
  })

  it('throws a status-based message when a non-ok body is not JSON', async () => {
    fetchMock.mockResolvedValue(
      makeResponse({
        ok: false,
        status: 500,
        json: () => Promise.reject(new SyntaxError('Unexpected token')),
      }),
    )

    await expect(apiRequest('/tracks/1')).rejects.toThrow('Request failed (500)')
  })

  it('throws when a 2xx body is not valid JSON', async () => {
    fetchMock.mockResolvedValue(
      makeResponse({
        ok: true,
        status: 204,
        json: () => Promise.reject(new SyntaxError('Unexpected end of input')),
      }),
    )

    await expect(apiRequest('/tracks/1')).rejects.toThrow(
      'Response was not valid JSON (204)',
    )
  })

  it('sets Content-Type only when a body is present', async () => {
    fetchMock.mockResolvedValue(
      makeResponse({ json: () => Promise.resolve({}) }),
    )

    await apiRequest('/tracks/1', {
      method: 'PATCH',
      body: JSON.stringify({ title: 'x' }),
    })
    const withBodyHeaders = fetchMock.mock.calls[0][1]?.headers as Headers
    expect(withBodyHeaders.get('Content-Type')).toBe('application/json')

    fetchMock.mockClear()
    await apiRequest('/tracks/1')
    const noBodyHeaders = fetchMock.mock.calls[0][1]?.headers as Headers
    expect(noBodyHeaders.has('Content-Type')).toBe(false)
  })

  it('does not overwrite an explicit Content-Type header', async () => {
    fetchMock.mockResolvedValue(
      makeResponse({ json: () => Promise.resolve({}) }),
    )

    await apiRequest('/tracks/1', {
      method: 'POST',
      body: 'a,b,c',
      headers: { 'Content-Type': 'text/csv' },
    })
    const headers = fetchMock.mock.calls[0][1]?.headers as Headers
    expect(headers.get('Content-Type')).toBe('text/csv')
  })
})
