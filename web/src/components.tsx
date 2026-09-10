import { AlertTriangle, ChevronRight, Copy, LoaderCircle, X } from 'lucide-react'
import { useState, type PropsWithChildren, type ReactNode } from 'react'
import type { RangeKey } from './time'

export function Page({ title, actions, children }: PropsWithChildren<{ title: string; actions?: ReactNode }>) {
  return <main className="page"><header className="page-header"><h1>{title}</h1>{actions}</header>{children}</main>
}

export function Status({ value }: { value?: string | null }) {
  const label = value ?? 'unavailable'
  return <span className={`status status-${label}`}>{label.replaceAll('_', ' ')}</span>
}

export function Loading() {
  return <div className="state"><LoaderCircle className="spin" size={20} /> Loading</div>
}

export function ErrorState({ error }: { error: Error }) {
  return <div className="state error"><AlertTriangle size={20} /> {error.message}</div>
}

export function Empty({ children = 'No records in this range.' }: PropsWithChildren) {
  return <div className="state">{children}</div>
}

export function Unavailable({ reason }: { reason?: string }) {
  return <div className="unavailable"><AlertTriangle size={16} /><span>{reason ?? 'Data source unavailable.'}</span></div>
}

export function RangePicker({ value, onChange, allowCustom = false }: { value: RangeKey; onChange: (value: RangeKey) => void; allowCustom?: boolean }) {
  const options: [RangeKey, string][] = [['today', 'Today'], ['7d', '7 days'], ['30d', '30 days']]
  if (allowCustom) options.push(['custom', 'Custom'])
  return <div className="segments" aria-label="Report range">
    {options.map(([key, label]) =>
      <button key={key} className={value === key ? 'active' : ''} onClick={() => onChange(key)}>{label}</button>)}
  </div>
}

export function DataTable({ columns, rows, onRow }: {
  columns: { key: string; label: string; render?: (row: any) => ReactNode }[]
  rows: any[]
  onRow?: (row: any) => void
}) {
  if (!rows.length) return <Empty />
  return <div className="table-scroll"><table><thead><tr>{columns.map((column) => <th key={column.key}>{column.label}</th>)}{onRow && <th aria-label="Open" />}</tr></thead>
    <tbody>{rows.map((row, index) => <tr key={row.id ?? index} className={onRow ? 'clickable' : ''} onClick={() => onRow?.(row)}>
      {columns.map((column) => <td key={column.key} data-label={column.label}>{column.render ? column.render(row) : String(row[column.key] ?? 'Unavailable')}</td>)}
      {onRow && <td><ChevronRight size={16} /></td>}
    </tr>)}</tbody></table></div>
}

export function SecretDialog({ secret, onClose }: { secret: string; onClose: () => void }) {
  const [copied, setCopied] = useState(false)
  const copy = async () => { await navigator.clipboard.writeText(secret); setCopied(true) }
  return <div className="dialog-backdrop" role="presentation"><section className="dialog" role="dialog" aria-modal="true" aria-labelledby="secret-title">
    <header><h2 id="secret-title">One-time credential</h2><button className="icon-button" onClick={onClose} title="Close"><X /></button></header>
    <p>This value is shown once and cannot be recovered.</p>
    <code className="secret">{secret}</code>
    <footer><button className="secondary" onClick={copy}><Copy size={16} />{copied ? 'Copied' : 'Copy'}</button><button onClick={onClose}>Done</button></footer>
  </section></div>
}
