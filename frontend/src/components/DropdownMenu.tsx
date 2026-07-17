import { useEffect, useId, useRef, useState } from 'react'
import type { KeyboardEvent } from 'react'

export type DropdownMenuItem = {
  label: string
  onSelect: () => void
  disabled?: boolean
}

/**
 * Accessible overflow menu (WAI-ARIA menu button pattern).
 *  - trigger carries aria-haspopup="menu" + aria-expanded
 *  - Enter / Space / ArrowDown open at the first item, ArrowUp opens at the last
 *  - ArrowUp / ArrowDown roam items (wrapping), Home / End jump to ends
 *  - Escape and Tab close and restore focus to the trigger
 *  - pointer-down outside closes without moving focus
 * Disabled items stay focusable (aria-disabled) so their presence is discoverable,
 * but selecting them is a no-op.
 */
export function DropdownMenu({
  label,
  items,
  buttonClassName = 'btn btn--secondary btn--sm',
}: {
  label: string
  items: DropdownMenuItem[]
  buttonClassName?: string
}) {
  const [open, setOpen] = useState(false)
  const [activeIndex, setActiveIndex] = useState(0)
  const menuId = useId()
  const containerRef = useRef<HTMLDivElement | null>(null)
  const triggerRef = useRef<HTMLButtonElement | null>(null)
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([])

  useEffect(() => {
    if (!open) {
      return
    }
    function onPointerDown(event: MouseEvent) {
      if (!containerRef.current?.contains(event.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', onPointerDown)
    return () => document.removeEventListener('mousedown', onPointerDown)
  }, [open])

  useEffect(() => {
    if (open) {
      itemRefs.current[activeIndex]?.focus()
    }
  }, [open, activeIndex])

  function openMenu(index: number) {
    setActiveIndex(index)
    setOpen(true)
  }

  function closeMenu(returnFocus: boolean) {
    setOpen(false)
    if (returnFocus) {
      triggerRef.current?.focus()
    }
  }

  function onTriggerKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (event.key === 'Enter' || event.key === ' ' || event.key === 'ArrowDown') {
      event.preventDefault()
      openMenu(0)
    } else if (event.key === 'ArrowUp') {
      event.preventDefault()
      openMenu(items.length - 1)
    }
  }

  function onItemKeyDown(event: KeyboardEvent<HTMLButtonElement>, index: number) {
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      setActiveIndex((index + 1) % items.length)
    } else if (event.key === 'ArrowUp') {
      event.preventDefault()
      setActiveIndex((index - 1 + items.length) % items.length)
    } else if (event.key === 'Home') {
      event.preventDefault()
      setActiveIndex(0)
    } else if (event.key === 'End') {
      event.preventDefault()
      setActiveIndex(items.length - 1)
    } else if (event.key === 'Escape' || event.key === 'Tab') {
      event.preventDefault()
      closeMenu(true)
    }
  }

  function selectItem(item: DropdownMenuItem) {
    if (item.disabled) {
      return
    }
    closeMenu(true)
    item.onSelect()
  }

  return (
    <div className="dropdown" ref={containerRef}>
      <button
        aria-controls={open ? menuId : undefined}
        aria-expanded={open}
        aria-haspopup="menu"
        className={buttonClassName}
        onClick={() => (open ? closeMenu(false) : openMenu(0))}
        onKeyDown={onTriggerKeyDown}
        ref={triggerRef}
        type="button"
      >
        {label}
      </button>
      {open ? (
        <div className="dropdown__menu" id={menuId} role="menu">
          {items.map((item, index) => (
            <button
              aria-disabled={item.disabled || undefined}
              className="dropdown__item"
              key={item.label}
              onClick={() => selectItem(item)}
              onKeyDown={(event) => onItemKeyDown(event, index)}
              ref={(node) => {
                itemRefs.current[index] = node
              }}
              role="menuitem"
              tabIndex={index === activeIndex ? 0 : -1}
              type="button"
            >
              {item.label}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  )
}
