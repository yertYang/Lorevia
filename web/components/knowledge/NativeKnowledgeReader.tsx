'use client'

import { useCallback, useEffect, useState } from 'react'
import { useRouter } from 'next/navigation'
import { useTranslation } from 'react-i18next'
import { Download, ExternalLink, Search, StickyNote } from 'lucide-react'
import { apiFetch, apiUrl } from '@/lib/api'
import { activeWorkspaceId, scopedUrl } from '@/lib/workspace-scope'
import { pageActionClass } from '@/components/layout/FeaturePage'
import { readingChanged, readingRequest } from '@/lib/native-reading'
import NativeRetrievalSettings from './NativeRetrievalSettings'

type File = { name: string; sha256: string }
type Passage = { path: string; hash: string; content: string }
type Hit = {
  id: string
  source_path: string
  source_hash: string
  start_line: number
  end_line: number
  content: string
}

export default function NativeKnowledgeReader({
  kbName,
  revision,
}: {
  kbName: string
  revision: number
}) {
  const { t } = useTranslation()
  const router = useRouter()
  const [scope] = useState(activeWorkspaceId)
  const [files, setFiles] = useState<File[]>([])
  const [selected, setSelected] = useState<File | null>(null)
  const [passage, setPassage] = useState<Passage | null>(null)
  const [query, setQuery] = useState('')
  const [hits, setHits] = useState<Hit[] | null>(null)
  const [sourceFilter, setSourceFilter] = useState('')
  const [topK, setTopK] = useState(5)
  const [resultMode, setResultMode] = useState('')
  const [start, setStart] = useState(1)
  const [end, setEnd] = useState(1)
  const [offset, setOffset] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const base = `/api/knowledge-bases/${encodeURIComponent(kbName)}`
  const url = useCallback((path: string) => apiUrl(scopedUrl(base + path, scope)), [base, scope])
  const request = useCallback(
    async <T,>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> => {
      const response = await apiFetch(
        url(path),
        body === undefined
          ? { cache: 'no-store', signal }
          : {
              method: 'POST',
              headers: { 'content-type': 'application/json' },
              body: JSON.stringify(body),
              signal,
            }
      )
      const value = await response.json()
      if (!response.ok) throw new Error(value.detail || `Request failed (${response.status})`)
      return value
    },
    [url]
  )
  useEffect(() => {
    const controller = new AbortController()
    setSelected(null)
    setPassage(null)
    setHits(null)
    setError('')
    void request<{ files: File[] }>('/files', undefined, controller.signal)
      .then(value => setFiles(value.files))
      .catch(cause => {
        if (!controller.signal.aborted) setError(String(cause.message))
      })
    return () => controller.abort()
  }, [request, revision])
  useEffect(() => {
    if (!selected) return
    const controller = new AbortController()
    setPassage(null)
    setStart(1)
    setEnd(1)
    setOffset(0)
    setError('')
    const params = new URLSearchParams({ path: selected.name, hash: selected.sha256 })
    void request<Passage>(`/passage?${params}`, undefined, controller.signal)
      .then(setPassage)
      .catch(cause => {
        if (!controller.signal.aborted) setError(String(cause.message))
      })
    return () => controller.abort()
  }, [selected, request])
  const perform = async (operation: () => Promise<void>) => {
    setBusy(true)
    setError('')
    try {
      await operation()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }
  const sourceUrl = (path: string, hash: string, line = 1, original = false) =>
    url(`/${original ? 'original' : 'source'}?${new URLSearchParams({ path, hash })}`) +
    (original ? '' : `#L${line}`)
  const clip = (path: string, hash: string, first: number, last: number) =>
    perform(async () => {
      const page = await request<{ id: string }>('/clip', {
        path,
        hash,
        start_line: first,
        end_line: last,
      })
      router.push(scopedUrl(`/co-writer/${page.id}`, scope))
    })
  const lines = passage?.content.split('\n') ?? []
  return (
    <div className="flex h-full min-h-0 flex-col overflow-y-auto p-4 sm:p-6">
      <NativeRetrievalSettings url={url} revision={revision} />
      <form
        className="mb-4 flex flex-wrap gap-2"
        onSubmit={event => {
          event.preventDefault()
          void perform(async () => {
            const result = await request<{ chunks: Hit[]; retrieval_mode: string; rerank: boolean }>('/search', { query, top_k: topK, source_path: sourceFilter })
            setHits(result.chunks)
            setResultMode(`${result.retrieval_mode}${result.rerank ? ' · ' + t('Reranked') : ''}`)
          })
        }}
      >
        <input
          aria-label={t('Search source passages')}
          className="min-w-0 flex-1 rounded-lg border border-[var(--border)] bg-[var(--background)] px-3 py-2"
          value={query}
          onChange={event => setQuery(event.target.value)}
          placeholder={t('Search source passages')}
        />
        <select aria-label={t('Source filter')} value={sourceFilter} onChange={event => setSourceFilter(event.target.value)}
          className="max-w-[200px] rounded-lg border border-[var(--border)] bg-[var(--background)] px-2">
          <option value="">{t('All sources')}</option>
          {files.map(file => <option key={file.name} value={file.name}>{file.name}</option>)}
        </select>
        <label className="flex items-center gap-2 text-xs">{t('Results')}
          <select value={topK} onChange={event => setTopK(Number(event.target.value))}
            className="rounded-lg border border-[var(--border)] bg-[var(--background)] p-2">
            {[3, 5, 10].map(count => <option key={count} value={count}>{count}</option>)}
          </select>
        </label>
        <button className={pageActionClass()} disabled={busy || !query.trim()}>
          <Search size={16} />
          {t('Search')}
        </button>
      </form>
      <p className="mb-4 text-xs text-[var(--muted-foreground)]">
        {t(
          'Text is extracted locally. OCR passages are marked; verify important details against the original file.'
        )}
      </p>
      {error && (
        <p role="alert" className="mb-4 break-words text-sm text-red-600">
          {error}
        </p>
      )}
      {hits && (
        <section aria-label={t('Search results')} className="mb-6 space-y-3">
          <p className="text-xs text-[var(--muted-foreground)]">{resultMode}</p>
          {!hits.length && <p>{t('No results found')}</p>}
          {hits.map(hit => (
            <article key={hit.id} className="rounded-xl border border-[var(--border)] p-3">
              <a
                className="break-all text-sm underline"
                href={sourceUrl(hit.source_path, hit.source_hash, hit.start_line)}
                target="_blank"
                rel="noopener noreferrer"
              >
                {hit.source_path}:{hit.start_line}–{hit.end_line}
                <ExternalLink className="ml-1 inline" size={13} />
              </a>
              <pre className="my-2 whitespace-pre-wrap break-words text-sm">{hit.content}</pre>
              <button
                className={pageActionClass('secondary')}
                disabled={busy}
                onClick={() =>
                  void clip(hit.source_path, hit.source_hash, hit.start_line, hit.end_line)
                }
              >
                <StickyNote size={15} />
                {t('Save passage as note')}
              </button>
            </article>
          ))}
        </section>
      )}
      <div className="grid min-w-0 gap-4 md:grid-cols-[200px_minmax(0,1fr)]">
        <nav aria-label={t('Files')} className="space-y-1">
          {files.map(file => (
            <button
              key={file.name}
              onClick={() => setSelected(file)}
              aria-current={selected?.name === file.name ? 'page' : undefined}
              className={`block w-full break-all rounded-lg p-2 text-left text-sm ${selected?.name === file.name ? 'bg-[var(--accent)]' : 'hover:bg-[var(--muted)]'}`}
            >
              {file.name}
            </button>
          ))}
          {!files.length && (
            <p className="text-sm text-[var(--muted-foreground)]">{t('No files')}</p>
          )}
        </nav>
        {passage && (
          <section className="min-w-0">
            <div className="mb-3 flex flex-wrap items-center gap-3">
              <h3 className="break-all font-medium">{passage.path}</h3>
              <button
                className={pageActionClass('secondary')}
                disabled={busy}
                onClick={() => void perform(async () => {
                  const item = await readingRequest<{id: string}>(scope, '', {kb: kbName, path: passage.path, hash: passage.hash})
                  readingChanged()
                  router.push(scopedUrl(`/learning/reading/${item.id}`, scope))
                })}
              >
                {t('Add to reading library')}
              </button>
              <a
                className={pageActionClass('secondary')}
                href={sourceUrl(passage.path, passage.hash, 1, true)}
                download={passage.path.split('/').at(-1)}
              >
                <Download size={15} />
                {t('Download original')}
              </a>
              <a
                className="text-sm underline"
                href={sourceUrl(passage.path, passage.hash)}
                target="_blank"
                rel="noopener noreferrer"
              >
                {t('Open source')}
              </a>
            </div>
            <div className="mb-3 flex flex-wrap items-end gap-2">
              <label className="text-xs">
                {t('Start line')}
                <input
                  type="number"
                  min={1}
                  max={lines.length}
                  value={start}
                  onChange={event => setStart(Number(event.target.value))}
                  className="ml-2 w-20 rounded border border-[var(--border)] bg-[var(--background)] p-1"
                />
              </label>
              <label className="text-xs">
                {t('End line')}
                <input
                  type="number"
                  min={start}
                  max={Math.min(lines.length, start + 499)}
                  value={end}
                  onChange={event => setEnd(Number(event.target.value))}
                  className="ml-2 w-20 rounded border border-[var(--border)] bg-[var(--background)] p-1"
                />
              </label>
              <button
                className={pageActionClass('secondary')}
                disabled={
                  busy || start < 1 || end < start || end > lines.length || end - start >= 500
                }
                onClick={() => void clip(passage.path, passage.hash, start, end)}
              >
                {t('Save passage as note')}
              </button>
            </div>
            <div className="rounded-lg border border-[var(--border)] p-3 font-mono text-sm">
              {lines.slice(offset, offset + 200).map((line, index) => (
                <div key={offset + index} className="flex gap-3">
                  <span className="w-10 shrink-0 text-right text-[var(--muted-foreground)]">
                    {offset + index + 1}
                  </span>
                  <span className="min-w-0 whitespace-pre-wrap break-words">{line || ' '}</span>
                </div>
              ))}
            </div>
            <div className="mt-3 flex gap-2">
              <button
                className={pageActionClass('secondary')}
                disabled={offset === 0}
                onClick={() => setOffset(Math.max(0, offset - 200))}
              >
                {t('Previous')}
              </button>
              <button
                className={pageActionClass('secondary')}
                disabled={offset + 200 >= lines.length}
                onClick={() => setOffset(offset + 200)}
              >
                {t('Next')}
              </button>
            </div>
          </section>
        )}
      </div>
    </div>
  )
}
