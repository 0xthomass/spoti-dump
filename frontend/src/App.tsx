import { useEffect, useRef, useState } from 'react'
import { Navigate, Route, Routes } from 'react-router-dom'
import './App.css'
import { ConfirmContext, RuntimeContext } from './context/runtime'
import type { ConfirmRequest, ConfirmState, Runtime } from './context/runtime'
import { NoticeBridge } from './components/NoticeBridge'
import { Shell } from './components/Shell'
import { Toast } from './components/Toast'
import { ConfirmModal } from './components/modals/ConfirmModal'
import { OperationModal } from './components/modals/OperationModal'
import { IdentityConflictsPage } from './pages/IdentityConflictsPage'
import { IdentityGapsPage } from './pages/IdentityGapsPage'
import { OverviewPage } from './pages/OverviewPage'
import { PlaylistsPage } from './pages/PlaylistsPage'
import { SafetyPage } from './pages/SafetyPage'
import { SavedTracksPage } from './pages/SavedTracksPage'
import { TracksPage } from './pages/TracksPage'

function App() {
  const [revision, setRevision] = useState(0)
  const [toast, setToast] = useState<string | null>(null)
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null)
  const [activeOperationId, setActiveOperationId] = useState<string | null>(null)
  const confirmResolverRef = useRef<((accepted: boolean) => void) | null>(null)

  useEffect(() => {
    if (!toast) {
      return
    }
    const timeout = window.setTimeout(() => {
      setToast(null)
    }, 3200)
    return () => window.clearTimeout(timeout)
  }, [toast])

  useEffect(() => {
    return () => {
      confirmResolverRef.current?.(false)
      confirmResolverRef.current = null
    }
  }, [])

  function closeConfirm(accepted: boolean) {
    const resolver = confirmResolverRef.current
    confirmResolverRef.current = null
    setConfirmState(null)
    resolver?.(accepted)
  }

  useEffect(() => {
    if (!confirmState) {
      return
    }

    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        closeConfirm(false)
      }
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => window.removeEventListener('keydown', handleKeyDown)
  }, [confirmState])

  async function confirm(request: ConfirmRequest) {
    if (confirmResolverRef.current) {
      confirmResolverRef.current(false)
      confirmResolverRef.current = null
    }

    setConfirmState({
      ...request,
      tone: request.tone ?? 'danger',
    })

    return await new Promise<boolean>((resolve) => {
      confirmResolverRef.current = resolve
    })
  }

  const runtime: Runtime = {
    revision,
    refresh() {
      setRevision((current) => current + 1)
    },
    notify(message) {
      setToast(message)
    },
    openOperation(operationId) {
      setActiveOperationId(operationId)
    },
  }

  return (
    <RuntimeContext.Provider value={runtime}>
      <ConfirmContext.Provider value={{ confirm }}>
        <Shell>
          <NoticeBridge />
          <Routes>
            <Route path="/" element={<Navigate replace to="/saved-tracks" />} />
            <Route path="/overview" element={<OverviewPage />} />
            <Route path="/saved-tracks" element={<SavedTracksPage />} />
            <Route path="/tracks" element={<TracksPage />} />
            <Route path="/identity-conflicts" element={<IdentityConflictsPage />} />
            <Route path="/identity-gaps" element={<IdentityGapsPage />} />
            <Route path="/playlists" element={<PlaylistsPage />} />
            <Route path="/playlists/:playlistId" element={<PlaylistsPage />} />
            <Route path="/safety" element={<SafetyPage />} />
            <Route path="/database" element={<Navigate replace to="/overview" />} />
          </Routes>
          {toast ? <Toast message={toast} /> : null}
          {confirmState ? (
            <ConfirmModal
              request={confirmState}
              onCancel={() => closeConfirm(false)}
              onConfirm={() => closeConfirm(true)}
            />
          ) : null}
          {activeOperationId ? (
            <OperationModal
              operationId={activeOperationId}
              onClose={() => setActiveOperationId(null)}
            />
          ) : null}
        </Shell>
      </ConfirmContext.Provider>
    </RuntimeContext.Provider>
  )
}

export default App
