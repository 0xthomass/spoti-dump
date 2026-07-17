import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Navigate, Route, Routes } from 'react-router-dom'
import './styles/tokens.css'
import './styles/base.css'
import './styles/layout.css'
import './styles/components.css'
import './styles/pages.css'
import { ConfirmContext, RuntimeContext } from './context/runtime'
import type {
  ConfirmApi,
  ConfirmRequest,
  ConfirmState,
  Notice,
  NotifyOptions,
  Runtime,
} from './context/runtime'
import { useOperationTracker } from './hooks/useOperationTracker'
import { ErrorBoundary } from './components/ErrorBoundary'
import { NoticeBridge } from './components/NoticeBridge'
import { NoticeStack } from './components/NoticeStack'
import { OperationTray } from './components/OperationTray'
import { Shell } from './components/Shell'
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
  const [notices, setNotices] = useState<Notice[]>([])
  const [confirmState, setConfirmState] = useState<ConfirmState | null>(null)
  const [activeOperationId, setActiveOperationId] = useState<string | null>(null)
  const confirmResolverRef = useRef<((accepted: boolean) => void) | null>(null)
  const noticeIdRef = useRef(0)

  const refresh = useCallback(() => {
    setRevision((current) => current + 1)
  }, [])

  const notify = useCallback((message: string, options?: NotifyOptions) => {
    const tone = options?.tone ?? 'info'
    setNotices((current) => {
      const next = [...current, { id: (noticeIdRef.current += 1), message, tone }]
      // Cap the visible stack; drop the oldest beyond three.
      return next.slice(-3)
    })
  }, [])

  const dismissNotice = useCallback((id: number) => {
    setNotices((current) => current.filter((notice) => notice.id !== id))
  }, [])

  const { track, operations, runningOperations } = useOperationTracker({
    notify,
    refresh,
  })

  const openOperation = useCallback(
    (operationId: string) => {
      track(operationId)
      setActiveOperationId(operationId)
    },
    [track],
  )

  useEffect(() => {
    return () => {
      confirmResolverRef.current?.(false)
      confirmResolverRef.current = null
    }
  }, [])

  const closeConfirm = useCallback((accepted: boolean) => {
    const resolver = confirmResolverRef.current
    confirmResolverRef.current = null
    setConfirmState(null)
    resolver?.(accepted)
  }, [])

  const confirm = useCallback((request: ConfirmRequest) => {
    if (confirmResolverRef.current) {
      confirmResolverRef.current(false)
      confirmResolverRef.current = null
    }

    setConfirmState({
      ...request,
      tone: request.tone ?? 'danger',
    })

    return new Promise<boolean>((resolve) => {
      confirmResolverRef.current = resolve
    })
  }, [])

  const runtime = useMemo<Runtime>(
    () => ({ revision, refresh, notify, openOperation }),
    [revision, refresh, notify, openOperation],
  )
  const confirmApi = useMemo<ConfirmApi>(() => ({ confirm }), [confirm])

  return (
    <RuntimeContext.Provider value={runtime}>
      <ConfirmContext.Provider value={confirmApi}>
        <Shell>
          <NoticeBridge />
          <ErrorBoundary>
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
            </Routes>
          </ErrorBoundary>
          <div className="dock">
            {activeOperationId === null && runningOperations.length > 0 ? (
              <OperationTray
                operations={runningOperations}
                onOpen={(operationId) => setActiveOperationId(operationId)}
              />
            ) : null}
            <NoticeStack notices={notices} onDismiss={dismissNotice} />
          </div>
          {confirmState ? (
            <ConfirmModal
              request={confirmState}
              onCancel={() => closeConfirm(false)}
              onConfirm={() => closeConfirm(true)}
            />
          ) : null}
          {activeOperationId ? (
            <OperationModal
              operation={operations[activeOperationId]}
              onClose={() => setActiveOperationId(null)}
            />
          ) : null}
        </Shell>
      </ConfirmContext.Provider>
    </RuntimeContext.Provider>
  )
}

export default App
