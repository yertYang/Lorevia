'use client'

import { useCallback, useEffect, useState } from 'react'
import { useTranslation } from 'react-i18next'
import { apiFetch } from '@/lib/api'
import { pageActionClass } from '@/components/layout/FeaturePage'

type Config = { mode: string; rerank: boolean; candidate_k: number }
type IndexStatus = {
  config: Config
  status: string
  indexed_chunks: number
  total_chunks: number
  embedding_configured: boolean
  embedding_model: string | null
  error_code: string
  error_detail: string
  operation_id: string
}

export default function NativeRetrievalSettings({ url, revision }: {
  url: (path: string) => string
  revision: number
}) {
  const { t } = useTranslation()
  const [index, setIndex] = useState<IndexStatus | null>(null)
  const [config, setConfig] = useState<Config>({ mode: 'lexical', rerank: false, candidate_k: 30 })
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [saved, setSaved] = useState(false)
  const read = useCallback(async (signal?: AbortSignal) => {
    const response = await apiFetch(url('/retrieval'), { cache: 'no-store', signal })
    const value = await response.json()
    if (!response.ok) throw new Error(value.detail || `Request failed (${response.status})`)
    return value as IndexStatus
  }, [url])
  useEffect(() => {
    const controller = new AbortController()
    void read(controller.signal).then(value => {
      setIndex(value)
      setConfig(value.config)
    }).catch(cause => {
      if (!controller.signal.aborted) setError(cause.message)
    })
    return () => controller.abort()
  }, [read, revision])
  useEffect(() => {
    if (index?.status !== 'building') return
    const controller = new AbortController()
    const timer = setInterval(() => {
      void read(controller.signal).then(setIndex).catch(cause => {
        if (!controller.signal.aborted) setError(cause.message)
      })
    }, 3000)
    return () => { clearInterval(timer); controller.abort() }
  }, [index?.status, read])
  const mutate = async (path: string, method: string, body?: Config) => {
    setBusy(true)
    setError('')
    setSaved(false)
    try {
      const response = await apiFetch(url(path), {
        method,
        ...(body ? { headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) } : {}),
      })
      const value = await response.json()
      if (!response.ok) throw new Error(value.detail || `Request failed (${response.status})`)
      setIndex(await read())
      if (path === '/retrieval') setSaved(true)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }
  const statusLabels: Record<string, string> = {
    not_indexed: t('Not indexed'), building: t('Building index'), ready: t('Index ready'),
    stale: t('Index outdated — rebuild required'), failed: t('Index build failed'),
    interrupted: t('Index build interrupted — retry required'),
  }
  const fieldClass = 'rounded-lg border border-[var(--border)] bg-[var(--background)] px-2 py-1.5'
  return (
    <details className="mb-4 rounded-xl border border-[var(--border)] p-3">
      <summary className="cursor-pointer text-sm font-medium">{t('Advanced retrieval')}</summary>
      <div className="mt-3 space-y-3 text-sm">
        <p className="text-xs text-[var(--muted-foreground)]">{t('Saved retrieval settings also apply to knowledge base searches in chat.')}</p>
        <div className="flex flex-wrap items-center gap-3">
          <label className="flex items-center gap-2">
            {t('Retrieval mode')}
            <select className={fieldClass} value={config.mode} disabled={!index || busy}
              onChange={event => { setSaved(false); setConfig({ ...config, mode: event.target.value }) }}>
              <option value="lexical">{t('Keyword search')}</option>
              <option value="vector">{t('Semantic search')}</option>
              <option value="hybrid">{t('Hybrid search')}</option>
            </select>
          </label>
          <label className="flex items-center gap-2">
            {t('Candidate passages')}
            <input className={`${fieldClass} w-20`} type="number" min={10} max={50}
              value={config.candidate_k} disabled={!index || busy}
              onChange={event => { setSaved(false); setConfig({ ...config, candidate_k: Number(event.target.value) }) }} />
          </label>
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={config.rerank} disabled={!index || busy}
              onChange={event => { setSaved(false); setConfig({ ...config, rerank: event.target.checked }) }} />
            {t('Rerank with current language model')}
          </label>
          <button type="button" className={pageActionClass('secondary')}
            disabled={!index || busy || config.candidate_k < 10 || config.candidate_k > 50}
            onClick={() => void mutate('/retrieval', 'PUT', config)}>{t('Save')}</button>
          {saved && <span role="status">{t('Saved')}</span>}
        </div>
        <p className="text-xs text-[var(--muted-foreground)]">{t('Semantic indexing sends document passages and search queries to the embedding service configured in Settings. Reranking sends candidate passages to the current language model and may incur usage charges.')}</p>
        {index && <div className="flex flex-wrap items-center gap-3">
          <span role="status">{statusLabels[index.status] || index.status} · {index.indexed_chunks}/{index.total_chunks}</span>
          {index.embedding_model && <span className="text-xs">{index.embedding_model}</span>}
          <button type="button" className={pageActionClass('secondary')}
            disabled={busy || index.status === 'building' || !index.embedding_configured || !index.total_chunks}
            onClick={() => void mutate('/reindex', 'POST')}>{t('Build / rebuild vector index')}</button>
          {!index.embedding_configured && <span className="text-xs">{t('Configure an embedding model in Settings first.')}</span>}
          {index.error_code && <span role="alert" className="break-words text-xs text-red-600">{t('Last index error')}: {index.error_detail || index.error_code} · {index.operation_id}</span>}
        </div>}
        {error && <p role="alert" className="break-words text-red-600">{error}</p>}
      </div>
    </details>
  )
}
