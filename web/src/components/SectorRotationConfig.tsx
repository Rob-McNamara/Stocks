import { useState } from 'react'
import type { SectorRotationSetConfig } from '../services/api'

/** Select value that switches the sector picker to a typed-in name. */
const NEW_SECTOR = '__new__'

type Draft = { symbol: string; sector: string; newSector: string }

const EMPTY_DRAFT: Draft = { symbol: '', sector: '', newSector: '' }

const normaliseSymbol = (symbol: string) => symbol.trim().toUpperCase()

/**
 * The sector names a set can use: the app's own list, plus any name a set
 * already uses that the list lacks. Saving the sets adds such a name to the
 * list on the server, so it only shows here first.
 */
function sectorChoices(list: string[], sets: SectorRotationSetConfig[]): string[] {
  const extra = sets.flatMap((s) => s.sectors.map((x) => x.sector)).filter((name) => !list.includes(name))
  return [...list, ...new Set(extra)]
}

/**
 * The Configuration screen's editor for the Dashboard's sector rotation sets.
 * Every change is saved straight away through `onSave`, which puts the old
 * sets back if the server refuses the new ones.
 */
export default function SectorRotationConfig({
  sets,
  sectorList,
  onSave,
}: {
  sets: SectorRotationSetConfig[]
  sectorList: string[]
  onSave: (next: SectorRotationSetConfig[]) => Promise<void>
}) {
  const [drafts, setDrafts] = useState<Record<string, Draft>>({})
  const [draftErrors, setDraftErrors] = useState<Record<string, string>>({})
  const [editing, setEditing] = useState<{ index: number; name: string; symbol: string; benchmarkName: string } | null>(null)
  const [editError, setEditError] = useState<string | null>(null)
  const [newSet, setNewSet] = useState({ name: '', symbol: '', benchmarkName: '' })
  const [newSetError, setNewSetError] = useState<string | null>(null)

  const choices = sectorChoices(sectorList, sets)
  const draftOf = (setName: string) => drafts[setName] ?? EMPTY_DRAFT
  const setDraft = (setName: string, patch: Partial<Draft>) => {
    setDrafts((d) => ({ ...d, [setName]: { ...(d[setName] ?? EMPTY_DRAFT), ...patch } }))
    setDraftErrors((e) => ({ ...e, [setName]: '' }))
  }
  const replaceSet = (index: number, next: SectorRotationSetConfig) => onSave(sets.map((s, i) => (i === index ? next : s)))
  const nameTaken = (name: string, except?: number) =>
    sets.some((s, i) => i !== except && s.name.trim().toLowerCase() === name.trim().toLowerCase())

  const addSector = async (index: number) => {
    const set = sets[index]
    const draft = draftOf(set.name)
    const symbol = normaliseSymbol(draft.symbol)
    const typed = draft.newSector.trim()
    // A typed name that only differs in case from a listed one is that one —
    // otherwise "technology" would sit in the list beside "Technology".
    const sector =
      draft.sector === NEW_SECTOR ? (choices.find((c) => c.toLowerCase() === typed.toLowerCase()) ?? typed) : draft.sector
    const fail = (message: string) => setDraftErrors((e) => ({ ...e, [set.name]: message }))
    if (!symbol) return fail('Enter a code')
    if (!sector) return fail(draft.sector === NEW_SECTOR ? 'Enter the new sector’s name' : 'Choose a sector')
    if (symbol === normaliseSymbol(set.benchmark.symbol)) return fail(`${symbol} is this set’s benchmark`)
    if (set.sectors.some((s) => normaliseSymbol(s.symbol) === symbol)) return fail(`${symbol} is already in this set`)
    setDrafts((d) => ({ ...d, [set.name]: EMPTY_DRAFT }))
    await replaceSet(index, { ...set, sectors: [...set.sectors, { symbol, sector }] })
  }

  const saveEdit = async () => {
    if (!editing) return
    const set = sets[editing.index]
    const name = editing.name.trim()
    const symbol = normaliseSymbol(editing.symbol)
    if (!name) return setEditError('Enter a name')
    if (nameTaken(name, editing.index)) return setEditError(`There is already a set called ${name}`)
    if (!symbol) return setEditError('Enter the benchmark’s code')
    if (set.sectors.some((s) => normaliseSymbol(s.symbol) === symbol)) return setEditError(`${symbol} is one of this set’s sectors`)
    setEditing(null)
    // Keep the add-row draft with the set if it was renamed.
    if (name !== set.name) setDrafts((d) => ({ ...d, [name]: d[set.name] ?? EMPTY_DRAFT }))
    await replaceSet(editing.index, { ...set, name, benchmark: { symbol, name: editing.benchmarkName.trim() } })
  }

  const addSet = async () => {
    const name = newSet.name.trim()
    const symbol = normaliseSymbol(newSet.symbol)
    if (!name) return setNewSetError('Enter a name for the set')
    if (nameTaken(name)) return setNewSetError(`There is already a set called ${name}`)
    if (!symbol) return setNewSetError('Enter the benchmark’s code')
    setNewSet({ name: '', symbol: '', benchmarkName: '' })
    await onSave([...sets, { name, benchmark: { symbol, name: newSet.benchmarkName.trim() }, sectors: [] }])
  }

  const removeSet = async (index: number) => {
    const set = sets[index]
    if (!confirm(`Remove the ${set.name} set and its ${set.sectors.length} sectors?`)) return
    setEditing(null)
    await onSave(sets.filter((_, i) => i !== index))
  }

  const moveUp = (index: number) => {
    const next = [...sets]
    ;[next[index - 1], next[index]] = [next[index], next[index - 1]]
    setEditing(null)
    void onSave(next)
  }

  const errorText = (message: string | null | undefined) =>
    message ? <span style={{ fontSize: 13, color: '#c62828', alignSelf: 'center' }}>{message}</span> : null

  return (
    <div className="config-card">
      <h2>Sector Rotation</h2>
      <p style={{ fontSize: 13, color: '#666', marginBottom: 16 }}>
        Sets of sector ETFs or indices for the Dashboard's Sector Rotation map, each measured against its
        benchmark. The first set is the one the Dashboard opens on. Sector names come from the same list the
        Holdings and Watchlist screens use — a new name is added to that list.
      </p>

      {sets.map((set, index) => {
        const draft = draftOf(set.name)
        const isEditing = editing?.index === index
        return (
          <div key={set.name} className="rotation-config-set">
            {isEditing && editing ? (
              <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'flex-end', marginBottom: 12 }}>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>Set name</label>
                  <input
                    className="config-input"
                    style={{ width: 120 }}
                    value={editing.name}
                    onChange={(e) => { setEditing({ ...editing, name: e.target.value }); setEditError(null) }}
                  />
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>Benchmark code</label>
                  <input
                    className="config-input"
                    style={{ width: 110 }}
                    value={editing.symbol}
                    onChange={(e) => { setEditing({ ...editing, symbol: e.target.value.toUpperCase() }); setEditError(null) }}
                  />
                </div>
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>Benchmark name</label>
                  <input
                    className="config-input"
                    style={{ width: 160 }}
                    value={editing.benchmarkName}
                    onChange={(e) => setEditing({ ...editing, benchmarkName: e.target.value })}
                  />
                </div>
                <button className="btn btn-primary" onClick={() => void saveEdit()}>Save</button>
                <button className="btn btn-outline" onClick={() => { setEditing(null); setEditError(null) }}>Cancel</button>
                {errorText(editError)}
              </div>
            ) : (
              <div className="rotation-config-header">
                <div>
                  <strong style={{ fontSize: 15 }}>{set.name}</strong>
                  <span style={{ color: '#666', fontSize: 13, marginLeft: 12 }}>
                    Benchmark: <span className="rotation-config-benchmark">{set.benchmark.symbol}</span>
                    {set.benchmark.name ? ` — ${set.benchmark.name}` : ''}
                  </span>
                </div>
                <div style={{ display: 'flex', gap: 6 }}>
                  {index > 0 && (
                    <button className="btn btn-outline btn-small" onClick={() => moveUp(index)} title="Move this set up — the first set is the one the Dashboard opens on">
                      Move up
                    </button>
                  )}
                  <button
                    className="btn btn-outline btn-small"
                    onClick={() => {
                      setEditing({ index, name: set.name, symbol: set.benchmark.symbol, benchmarkName: set.benchmark.name })
                      setEditError(null)
                    }}
                  >
                    Edit
                  </button>
                  <button className="btn btn-danger btn-small" onClick={() => void removeSet(index)}>
                    Remove set
                  </button>
                </div>
              </div>
            )}

            {set.sectors.length > 0 && (
              <table className="holdings-table compact rotation-config-table" style={{ marginBottom: 12 }}>
                <thead>
                  <tr>
                    <th>Code</th>
                    <th>Sector</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {set.sectors.map((row, rowIndex) => (
                    <tr key={row.symbol}>
                      <td style={{ fontWeight: 600 }}>{row.symbol}</td>
                      <td>
                        <select
                          className="config-input"
                          value={row.sector}
                          aria-label={`Sector for ${row.symbol}`}
                          onChange={(e) =>
                            void replaceSet(index, {
                              ...set,
                              sectors: set.sectors.map((s, j) => (j === rowIndex ? { ...s, sector: e.target.value } : s)),
                            })
                          }
                        >
                          {choices.map((name) => (
                            <option key={name} value={name}>{name}</option>
                          ))}
                        </select>
                      </td>
                      <td style={{ width: 1 }}>
                        <button
                          className="btn btn-danger btn-small"
                          onClick={() => void replaceSet(index, { ...set, sectors: set.sectors.filter((_, j) => j !== rowIndex) })}
                        >
                          Remove
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}

            <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'flex-end' }}>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Code</label>
                <input
                  className="config-input"
                  style={{ width: 110 }}
                  value={draft.symbol}
                  placeholder={set.sectors[0]?.symbol ?? 'XLK'}
                  aria-label={`New sector code for ${set.name}`}
                  onChange={(e) => setDraft(set.name, { symbol: e.target.value.toUpperCase() })}
                />
              </div>
              <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                <label style={{ fontSize: 12, color: '#666' }}>Sector</label>
                <select
                  className="config-input"
                  value={draft.sector}
                  aria-label={`New sector for ${set.name}`}
                  onChange={(e) => setDraft(set.name, { sector: e.target.value })}
                >
                  <option value="">Select sector...</option>
                  {choices.map((name) => (
                    <option key={name} value={name}>{name}</option>
                  ))}
                  <option value={NEW_SECTOR}>New sector…</option>
                </select>
              </div>
              {draft.sector === NEW_SECTOR && (
                <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
                  <label style={{ fontSize: 12, color: '#666' }}>New sector name</label>
                  <input
                    className="config-input"
                    style={{ width: 170 }}
                    value={draft.newSector}
                    aria-label={`New sector name for ${set.name}`}
                    onChange={(e) => setDraft(set.name, { newSector: e.target.value })}
                  />
                </div>
              )}
              <button className="btn btn-primary" onClick={() => void addSector(index)}>
                Add Sector
              </button>
              {errorText(draftErrors[set.name])}
            </div>
          </div>
        )
      })}

      <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'flex-end', marginTop: sets.length > 0 ? 8 : 0 }}>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
          <label style={{ fontSize: 12, color: '#666' }}>New set name</label>
          <input
            className="config-input"
            style={{ width: 120 }}
            value={newSet.name}
            placeholder="e.g. Europe"
            onChange={(e) => { setNewSet({ ...newSet, name: e.target.value }); setNewSetError(null) }}
          />
        </div>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
          <label style={{ fontSize: 12, color: '#666' }}>Benchmark code</label>
          <input
            className="config-input"
            style={{ width: 110 }}
            value={newSet.symbol}
            placeholder="e.g. SPY"
            onChange={(e) => { setNewSet({ ...newSet, symbol: e.target.value.toUpperCase() }); setNewSetError(null) }}
          />
        </div>
        <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
          <label style={{ fontSize: 12, color: '#666' }}>Benchmark name</label>
          <input
            className="config-input"
            style={{ width: 160 }}
            value={newSet.benchmarkName}
            placeholder="e.g. S&P 500"
            onChange={(e) => setNewSet({ ...newSet, benchmarkName: e.target.value })}
          />
        </div>
        <button className="btn btn-primary" onClick={() => void addSet()}>
          Add Set
        </button>
        {errorText(newSetError)}
      </div>
    </div>
  )
}
