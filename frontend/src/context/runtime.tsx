import { createContext, useContext } from 'react'

export type Runtime = {
  revision: number
  refresh: () => void
  notify: (message: string) => void
  openOperation: (operationId: string) => void
}

export type ConfirmTone = 'danger' | 'warning'

export type ConfirmRequest = {
  title: string
  message: string
  confirmLabel: string
  details?: string
  tone?: ConfirmTone
}

export type ConfirmState = ConfirmRequest & {
  tone: ConfirmTone
}

export type ConfirmApi = {
  confirm: (request: ConfirmRequest) => Promise<boolean>
}

export const RuntimeContext = createContext<Runtime | null>(null)
export const ConfirmContext = createContext<ConfirmApi | null>(null)

export function useRuntime() {
  const value = useContext(RuntimeContext)
  if (!value) {
    throw new Error('Runtime context is unavailable')
  }
  return value
}

export function useConfirm() {
  const value = useContext(ConfirmContext)
  if (!value) {
    throw new Error('Confirm context is unavailable')
  }
  return value.confirm
}
