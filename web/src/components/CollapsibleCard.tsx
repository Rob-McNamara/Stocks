import { useEffect, useId, useState, type ReactNode } from 'react'
import { loadCollapsedCards, setCardCollapsed } from '../utils/collapsedCards'

/**
 * A `manager-card` whose body the user can fold away, remembered across visits.
 *
 * The folded state is stored server-side rather than held in component state:
 * the point of folding a card is to stop having to scroll past it, and a toggle
 * that reset every time the screen was mounted would have to be redone on every
 * visit. It opens expanded until the stored set says otherwise, so a config that
 * cannot be read costs nothing but a scroll.
 *
 * `id` is what is written to the stored set, so it must be stable across
 * releases — renaming one silently reopens a card the user had shut.
 */
export default function CollapsibleCard({
  id,
  title,
  className = 'manager-card',
  children,
}: {
  id: string
  title: string
  className?: string
  children: ReactNode
}) {
  const [collapsed, setCollapsed] = useState(false)
  const bodyId = useId()

  useEffect(() => {
    let cancelled = false
    loadCollapsedCards()
      .then((ids) => { if (!cancelled) setCollapsed(ids.has(id)) })
      .catch(() => { /* unreadable config — leave the card open */ })
    return () => { cancelled = true }
  }, [id])

  // Folded immediately, saved in the background: the fold is the whole point of
  // the click and must not wait on a round trip. A failed save costs the
  // preference, not the interaction.
  const toggle = () => {
    const next = !collapsed
    setCollapsed(next)
    setCardCollapsed(id, next).catch(() => {})
  }

  return (
    <div className={className}>
      <div className="card-header">
        <h2>
          <button
            type="button"
            className="card-collapse-toggle"
            onClick={toggle}
            aria-expanded={!collapsed}
            aria-controls={bodyId}
          >
            {/* The caret is decorative: `aria-expanded` on the button already
                states which way the card is folded. */}
            <span className="card-collapse-caret" aria-hidden="true">{collapsed ? '▸' : '▾'}</span>
            {title}
          </button>
        </h2>
      </div>
      {/* Unmounted rather than hidden, so a folded card costs no layout work —
          these wrap treemaps that lay out every holding on each render. */}
      {!collapsed && <div id={bodyId}>{children}</div>}
    </div>
  )
}
