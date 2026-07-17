import { useState } from 'react'
import { act, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { Notice } from '../context/runtime'
import { NoticeStack } from './NoticeStack'

function notice(overrides: Partial<Notice> = {}): Notice {
  return { id: 1, message: 'Saved changes', tone: 'success', ...overrides }
}

// Stateful host mirroring how App owns the notices list, so a dismiss actually
// removes the item from the DOM (NoticeStack itself is controlled).
function Host({ initial }: { initial: Notice[] }) {
  const [notices, setNotices] = useState(initial)
  return (
    <NoticeStack
      notices={notices}
      onDismiss={(id) => setNotices((current) => current.filter((n) => n.id !== id))}
    />
  )
}

afterEach(() => {
  vi.useRealTimers()
})

describe('NoticeStack', () => {
  it('renders a polite status container', () => {
    render(<NoticeStack notices={[notice()]} onDismiss={vi.fn()} />)
    const container = screen.getByRole('status')
    expect(container).toHaveClass('notice-stack')
    expect(container).toHaveAttribute('aria-live', 'polite')
  })

  it('renders every tone with its icon and dismiss control', () => {
    const notices: Notice[] = [
      { id: 1, message: 'All good', tone: 'success' },
      { id: 2, message: 'Something broke', tone: 'error' },
      { id: 3, message: 'Heads up', tone: 'info' },
    ]
    const { container } = render(<NoticeStack notices={notices} onDismiss={vi.fn()} />)

    for (const tone of ['success', 'error', 'info'] as const) {
      const el = container.querySelector(`.notice--${tone}`)
      expect(el).not.toBeNull()
      // Each notice carries an (aria-hidden) icon svg.
      expect(el?.querySelector('.notice__icon svg')).not.toBeNull()
    }
    expect(screen.getByText('All good')).toBeInTheDocument()
    expect(screen.getByText('Something broke')).toBeInTheDocument()
    expect(screen.getByText('Heads up')).toBeInTheDocument()
    expect(
      screen.getAllByRole('button', { name: 'Dismiss notification' }),
    ).toHaveLength(3)
  })

  it('removes a notice when its dismiss button is clicked', async () => {
    const user = userEvent.setup()
    render(
      <Host
        initial={[
          { id: 1, message: 'First', tone: 'info' },
          { id: 2, message: 'Second', tone: 'info' },
        ]}
      />,
    )
    expect(screen.getByText('First')).toBeInTheDocument()

    const firstNotice = screen.getByText('First').closest('.notice') as HTMLElement
    await user.click(
      within(firstNotice).getByRole('button', { name: 'Dismiss notification' }),
    )

    expect(screen.queryByText('First')).not.toBeInTheDocument()
    expect(screen.getByText('Second')).toBeInTheDocument()
  })

  it('auto-dismisses single-line success and info notices after 6s', () => {
    vi.useFakeTimers()
    const onDismissSuccess = vi.fn()
    const { rerender } = render(
      <NoticeStack
        notices={[notice({ id: 7, tone: 'success' })]}
        onDismiss={onDismissSuccess}
      />,
    )
    act(() => vi.advanceTimersByTime(5999))
    expect(onDismissSuccess).not.toHaveBeenCalled()
    act(() => vi.advanceTimersByTime(1))
    expect(onDismissSuccess).toHaveBeenCalledWith(7)

    const onDismissInfo = vi.fn()
    rerender(
      <NoticeStack
        notices={[notice({ id: 8, tone: 'info', message: 'FYI' })]}
        onDismiss={onDismissInfo}
      />,
    )
    act(() => vi.advanceTimersByTime(6000))
    expect(onDismissInfo).toHaveBeenCalledWith(8)
  })

  it('keeps error notices until dismissed manually', () => {
    vi.useFakeTimers()
    const onDismiss = vi.fn()
    render(
      <NoticeStack
        notices={[notice({ id: 9, tone: 'error', message: 'Broke' })]}
        onDismiss={onDismiss}
      />,
    )
    act(() => vi.advanceTimersByTime(60_000))
    expect(onDismiss).not.toHaveBeenCalled()
  })

  it('keeps multi-line notices persistent even when the tone auto-dismisses', () => {
    vi.useFakeTimers()
    const onDismiss = vi.fn()
    render(
      <NoticeStack
        notices={[notice({ id: 10, tone: 'success', message: 'Line one\nLine two' })]}
        onDismiss={onDismiss}
      />,
    )
    act(() => vi.advanceTimersByTime(60_000))
    expect(onDismiss).not.toHaveBeenCalled()
  })
})
