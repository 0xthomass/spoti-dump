import type { ReactNode } from 'react'
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { apiRequest } from '../../api/client'
import type { ActionResponse, TrackDetail } from '../../api/types'
import {
  ConfirmContext,
  RuntimeContext,
  type ConfirmApi,
  type Runtime,
} from '../../context/runtime'
import { TrackEditorModal } from './TrackEditorModal'

// Mock only apiRequest; keep actionMessage (and everything else) real so the
// success notification path exercises the true helper.
vi.mock('../../api/client', async (importActual) => {
  const actual = await importActual<typeof import('../../api/client')>()
  return { ...actual, apiRequest: vi.fn() }
})

const mockApiRequest = vi.mocked(apiRequest)

const notify = vi.fn()
const refresh = vi.fn()
const openOperation = vi.fn()
const onClose = vi.fn()
// The editor form has no dirty edits to discard in the happy path, so confirm
// is only reached by close-with-changes flows we don't drive here.
const confirm = vi.fn(async () => true)

function makeTrack(artists: string[]): TrackDetail {
  return {
    track_id: 't1',
    title: 'IFHY',
    artists,
    artist_summary: artists.join(', '),
    album: 'Wolf',
    duration_seconds: 220,
    duration_label: '3:40',
    isrc: null,
    coverage: { key: 'multi-provider', label: 'Multi-provider', short_label: 'Multi' },
    providers: [],
    provider_status: [],
    identity_conflicts: [],
    saved_count: 1,
    playlist_refs: 0,
    artwork_url: null,
  }
}

/** Route GET reads to the track detail and capture PATCH saves. */
function stubApi(track: TrackDetail) {
  const impl = (_path: string, init?: RequestInit) => {
    if (init?.method === 'PATCH') {
      return Promise.resolve<ActionResponse>({ message: 'Track updated.', warnings: [] })
    }
    return Promise.resolve(track)
  }
  mockApiRequest.mockImplementation(impl as unknown as typeof apiRequest)
}

function renderModal() {
  const runtime: Runtime = { revision: 0, refresh, notify, openOperation }
  const confirmApi: ConfirmApi = { confirm }
  return render(
    <RuntimeContext.Provider value={runtime}>
      <ConfirmContext.Provider value={confirmApi}>
        <TrackEditorModal trackId="t1" onClose={onClose} />
      </ConfirmContext.Provider>
    </RuntimeContext.Provider> as ReactNode,
  )
}

function lastPatchBody(): Record<string, unknown> {
  const patchCall = [...mockApiRequest.mock.calls]
    .reverse()
    .find(([, init]) => (init as RequestInit | undefined)?.method === 'PATCH')
  if (!patchCall) {
    throw new Error('no PATCH call recorded')
  }
  const body = (patchCall[1] as RequestInit).body as string
  return JSON.parse(body)
}

beforeEach(() => {
  mockApiRequest.mockReset()
  notify.mockReset()
  refresh.mockReset()
  onClose.mockReset()
  confirm.mockClear()
})

describe('TrackEditorModal artist editor', () => {
  it('preserves a comma-containing artist name verbatim on save', async () => {
    stubApi(makeTrack(['Tyler, The Creator']))
    const user = userEvent.setup()
    renderModal()

    // Form seeded from the loaded track detail.
    const artistInput = await screen.findByLabelText('Artist 1')
    expect(artistInput).toHaveValue('Tyler, The Creator')

    await user.click(screen.getByRole('button', { name: 'Save Track' }))

    await waitFor(() => expect(onClose).toHaveBeenCalled())
    // The regression guard: the single artist survives as one entry, comma intact.
    expect(lastPatchBody().artists).toEqual(['Tyler, The Creator'])
    expect(refresh).toHaveBeenCalled()
  })

  it('adds a new artist row and includes it in the payload', async () => {
    stubApi(makeTrack(['Tyler, The Creator']))
    const user = userEvent.setup()
    renderModal()

    await screen.findByLabelText('Artist 1')
    await user.click(screen.getByRole('button', { name: 'Add artist' }))

    const secondRow = await screen.findByLabelText('Artist 2')
    await user.type(secondRow, 'Kali Uchis')

    await user.click(screen.getByRole('button', { name: 'Save Track' }))

    await waitFor(() => expect(onClose).toHaveBeenCalled())
    expect(lastPatchBody().artists).toEqual(['Tyler, The Creator', 'Kali Uchis'])
  })

  it('removes an artist row and drops it from the payload', async () => {
    stubApi(makeTrack(['Tyler, The Creator', 'Kali Uchis']))
    const user = userEvent.setup()
    renderModal()

    await screen.findByLabelText('Artist 2')
    await user.click(screen.getByRole('button', { name: 'Remove artist 2' }))

    await waitFor(() =>
      expect(screen.queryByLabelText('Artist 2')).not.toBeInTheDocument(),
    )
    await user.click(screen.getByRole('button', { name: 'Save Track' }))

    await waitFor(() => expect(onClose).toHaveBeenCalled())
    expect(lastPatchBody().artists).toEqual(['Tyler, The Creator'])
  })

  it('trims blank artist rows out of the payload', async () => {
    stubApi(makeTrack(['Tyler, The Creator']))
    const user = userEvent.setup()
    renderModal()

    await screen.findByLabelText('Artist 1')
    // Add an empty row and leave it blank.
    await user.click(screen.getByRole('button', { name: 'Add artist' }))
    await screen.findByLabelText('Artist 2')

    await user.click(screen.getByRole('button', { name: 'Save Track' }))

    await waitFor(() => expect(onClose).toHaveBeenCalled())
    expect(lastPatchBody().artists).toEqual(['Tyler, The Creator'])
  })
})
