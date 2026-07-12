import { NavLink } from 'react-router-dom'

export function SidebarLink({
  to,
  label,
  copy,
}: {
  to: string
  label: string
  copy: string
}) {
  return (
    <NavLink
      className={({ isActive }) =>
        `sidebar-link${isActive ? ' sidebar-link--active' : ''}`
      }
      to={to}
    >
      <strong>{label}</strong>
      <span>{copy}</span>
    </NavLink>
  )
}
