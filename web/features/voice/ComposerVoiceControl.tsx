'use client'

import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { Loader2, Mic, PhoneOff, Square, Volume2, X } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { apiFetch, apiUrl, requestJson } from '@/lib/api'
import { useVoiceRecorder } from '@/hooks/useVoiceRecorder'
import Tooltip from '@/shared/ui/Tooltip'
import { SILENT_UNLOCK_WAV } from '@/lib/voice-settings'

export type VoiceReply = { key: string; content: string }
type Phase = 'idle' | 'checking' | 'waiting' | 'synthesizing' | 'playing'
type SpeechStatus = { configured: boolean; model?: string; message?: string }



export default function ComposerVoiceControl({
  busy, visible = true, conversationKey, reply, onTranscript, onSend, onVoiceTurn,
}: {
  busy: boolean
  visible?: boolean
  conversationKey?: string
  reply?: VoiceReply
  onTranscript: (text: string) => void
  onSend?: (text: string) => boolean
  onVoiceTurn?: () => void
}) {
  const { t } = useTranslation()
  const [open, setOpen] = useState(false)
  const [mode, setMode] = useState<'conversation' | 'input'>(onSend ? 'conversation' : 'input')
  const [active, setActive] = useState(false)
  const [phase, setPhase] = useState<Phase>('idle')
  const [error, setError] = useState<string | null>(null)
  const [models, setModels] = useState<{ stt: SpeechStatus; tts: SpeechStatus } | null>(null)
  const [transcript, setTranscript] = useState('')
  const root = useRef<HTMLDivElement>(null)
  const panel = useRef<HTMLElement>(null)
  const trigger = useRef<HTMLButtonElement>(null)
  const panelId = useId()
  const [position, setPosition] = useState({ left: 16, top: 16 })
  const activeRef = useRef(false)
  const request = useRef<AbortController | null>(null)
  const audio = useRef<HTMLAudioElement | null>(null)
  const audioUrl = useRef<string | null>(null)
  const beforeReply = useRef<VoiceReply | undefined>(undefined)
  const sawBusy = useRef(false)

  const recorder = useVoiceRecorder(text => {
    if (!activeRef.current) { onTranscript(text); return }
    setTranscript(text)
    beforeReply.current = reply
    sawBusy.current = false
    if (onSend?.(text)) { onVoiceTurn?.(); setPhase('waiting') }
    else { onTranscript(text); setPhase('idle') }
  })

  const stopPlayback = useCallback(() => {
    request.current?.abort()
    request.current = null
    if (audio.current) { audio.current.pause(); audio.current.src = ''; audio.current = null }
    if (audioUrl.current) { URL.revokeObjectURL(audioUrl.current); audioUrl.current = null }
  }, [])

  const end = useCallback(() => {
    activeRef.current = false
    setActive(false)
    recorder.cancel()
    stopPlayback()
    setPhase('idle')
    setError(null)
  }, [recorder.cancel, stopPlayback])

  const close = useCallback(() => { end(); setOpen(false); trigger.current?.focus() }, [end])

  useLayoutEffect(() => {
    if (!open || !panel.current) return
    const place = () => {
      const anchor = root.current?.getBoundingClientRect()
      const popup = panel.current?.getBoundingClientRect()
      if (!anchor || !popup) return
      const view = window.visualViewport
      const width = view?.width ?? window.innerWidth
      const height = view?.height ?? window.innerHeight
      const top = view?.offsetTop ?? 0
      const next = {
        left: Math.max(16, Math.min(anchor.right - popup.width, width - popup.width - 16)),
        top: Math.max(top + 16, Math.min(anchor.top - popup.height - 12, top + height - popup.height - 16)),
      }
      setPosition(current => current.left === next.left && current.top === next.top ? current : next)
    }
    place()
    panel.current.focus({ preventScroll: true })
    const observer = new ResizeObserver(place)
    observer.observe(panel.current)
    // The first sent message moves the composer to the bottom of the page.
    // Its position can animate without either element changing size.
    let frame = 0
    const followAnchor = () => { place(); frame = requestAnimationFrame(followAnchor) }
    frame = requestAnimationFrame(followAnchor)
    window.addEventListener('resize', place)
    window.addEventListener('scroll', place, true)
    window.visualViewport?.addEventListener('resize', place)
    return () => {
      observer.disconnect()
      cancelAnimationFrame(frame)
      window.removeEventListener('resize', place)
      window.removeEventListener('scroll', place, true)
      window.visualViewport?.removeEventListener('resize', place)
    }
  }, [open])

  useEffect(() => {
    // A draft acquiring its first session id keeps the same composer. Its
    // pathname changes; only an actual session switch should end the call.
    return () => {
      activeRef.current = false
      stopPlayback()
    }
  }, [conversationKey, stopPlayback])

  const previousKey = useRef(conversationKey)
  useEffect(() => {
    if (previousKey.current !== conversationKey) { end(); setOpen(false) }
    previousKey.current = conversationKey
  }, [conversationKey, end])

  useEffect(() => {
    if (!open) return
    const onKey = (event: KeyboardEvent) => { if (event.key === 'Escape') close() }
    const onPointer = (event: PointerEvent) => {
      if (!activeRef.current && recorder.state === 'idle' && !root.current?.contains(event.target as Node) && !panel.current?.contains(event.target as Node)) close()
    }
    document.addEventListener('keydown', onKey)
    document.addEventListener('pointerdown', onPointer)
    window.addEventListener('deeptutor:before-workspace-switch', close)
    return () => {
      document.removeEventListener('keydown', onKey)
      document.removeEventListener('pointerdown', onPointer)
      window.removeEventListener('deeptutor:before-workspace-switch', close)
    }
  }, [close, open, recorder.state])

  const start = async () => {
    if (busy || phase !== 'idle') return
    setError(null)
    if (mode === 'input') { void recorder.start(); return }
    // Reuse this element after the reply arrives (required by WebKit).
    audio.current ??= new Audio(SILENT_UNLOCK_WAV)
    void audio.current.play().catch(() => {})
    const controller = new AbortController()
    request.current = controller
    setPhase('checking')
    try {
      const status = await requestJson<{ stt: SpeechStatus; tts: SpeechStatus }>(apiUrl('/api/voice/status'), {
        signal: AbortSignal.any([controller.signal, AbortSignal.timeout(15000)]),
      })
      if (controller.signal.aborted) return
      setModels(status)
      if (!status.stt.configured || !status.tts.configured)
        throw new Error(status.stt.message || status.tts.message || 'Configure speech models in Settings → Voice.')
      activeRef.current = true
      setActive(true)
      setPhase('idle')
      void recorder.start()
    } catch (err) {
      if (!controller.signal.aborted) {
        setError(t(err instanceof Error ? err.message : 'Could not start the voice conversation.'))
        setPhase('idle')
      }
    }
  }

  useEffect(() => {
    if (phase !== 'waiting' || !activeRef.current) return
    if (busy) { sawBusy.current = true; return }
    const changed = reply && (reply.key !== beforeReply.current?.key || reply.content !== beforeReply.current?.content)
    if (!changed || !reply.content.trim()) {
      if (sawBusy.current) { setPhase('idle'); setError(t('No voice reply was returned. Please try again.')) }
      return
    }
    const controller = new AbortController()
    request.current = controller
    setPhase('synthesizing')
    void (async () => {
      try {
        // The service accepts at most 4096 characters. Short sequential
        // requests also keep long replies from exceeding its timeout.
        const chars = Array.from(reply.content)
        for (let offset = 0; offset < chars.length; offset += 1500) {
          const response = await apiFetch(apiUrl('/api/voice/tts'), {
            method: 'POST', headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ text: chars.slice(offset, offset + 1500).join('') }),
            signal: AbortSignal.any([controller.signal, AbortSignal.timeout(130000)]),
          })
          if (!response.ok) {
            const detail = await response.json().catch(() => ({}))
            throw new Error(detail.message || 'Speech playback failed. Check the speech model settings and provider connection.')
          }
          const blob = await response.blob()
          if (controller.signal.aborted) return
          const player = audio.current ?? new Audio()
          audio.current = player
          if (audioUrl.current) URL.revokeObjectURL(audioUrl.current)
          audioUrl.current = URL.createObjectURL(blob)
          player.src = audioUrl.current
          setPhase('playing')
          await new Promise<void>((resolve, reject) => {
            const aborted = () => reject(new DOMException('Stopped', 'AbortError'))
            controller.signal.addEventListener('abort', aborted, { once: true })
            const done = (error?: Error) => {
              controller.signal.removeEventListener('abort', aborted)
              player.onended = null; player.onerror = null
              if (error) reject(error); else resolve()
            }
            player.onended = () => done()
            player.onerror = () => done(new Error('Speech playback failed. Check the speech model settings and provider connection.'))
            void player.play().catch(error => done(error))
          })
          if (controller.signal.aborted) return
          setPhase('synthesizing')
        }
        if (audioUrl.current) { URL.revokeObjectURL(audioUrl.current); audioUrl.current = null }
        setPhase('idle')
      } catch (err) {
        if (!controller.signal.aborted) {
          setError(t(err instanceof Error ? err.message : 'Speech playback failed. Check the speech model settings and provider connection.'))
          setPhase('idle')
        }
      }
    })()
    // end()/unmount own cancellation. Changing phase must not cancel playback.
  }, [busy, phase, reply, t])

  useEffect(() => {
    if (phase !== 'waiting') return
    const timer = setTimeout(() => {
      setPhase('idle'); setError(t('No voice reply was returned. Please try again.'))
    }, 180000)
    return () => clearTimeout(timer)
  }, [phase, t])

  if (!visible && !open && recorder.state === 'idle') return null
  const recording = recorder.state === 'recording'
  const working = phase !== 'idle' || recorder.state === 'checking' || recorder.state === 'transcribing'
  const status = recording ? 'Listening… Click to finish.' : recorder.state === 'transcribing' ? 'Transcribing…'
    : phase === 'waiting' ? 'Waiting for reply…' : phase === 'playing' ? 'Playing reply…'
    : phase === 'synthesizing' ? 'Preparing voice reply…' : phase === 'checking' ? 'Connecting…' : 'Click to speak'

  return <div ref={root} className="relative inline-flex shrink-0">
    <Tooltip label={t(onSend ? 'Voice conversation' : 'Record voice')} side="top" suppressed={open}>
      <button ref={trigger} type="button" aria-label={t(onSend ? 'Voice conversation' : 'Record voice')} aria-expanded={open} aria-controls={panelId}
        onClick={() => open ? close() : setOpen(true)}
        className={`inline-flex h-8 w-8 items-center justify-center rounded-[10px] transition-colors ${active || recording ? 'bg-red-500/15 text-red-500' : 'text-[var(--muted-foreground)] hover:bg-[var(--muted)]/55 hover:text-[var(--foreground)]'}`}>
        <Mic size={16} strokeWidth={1.9} />
      </button>
    </Tooltip>
    {open && createPortal(<section ref={panel} id={panelId} tabIndex={-1} aria-label={t('Voice conversation')}
      style={position}
      className="fixed z-[100] max-h-[calc(100dvh-32px)] w-[min(340px,calc(100vw-32px))] overflow-y-auto rounded-2xl border border-[var(--border)] bg-[var(--popover)] p-4 text-[var(--foreground)] shadow-xl outline-none">
      <div className="flex items-center justify-between gap-2">
        <div className="flex gap-1 rounded-lg bg-[var(--muted)]/50 p-0.5">
          {(onSend ? ['conversation', 'input'] as const : ['input'] as const).map(value => <button key={value} type="button"
            aria-pressed={mode === value} onClick={() => { end(); setMode(value) }}
            className={`rounded-md px-2.5 py-1 text-xs ${mode === value ? 'bg-[var(--background)] shadow-sm' : 'text-[var(--muted-foreground)]'}`}>
            {t(value === 'conversation' ? 'Voice conversation' : 'Voice input')}
          </button>)}
        </div>
        <button type="button" aria-label={t('Close')} onClick={close} className="rounded-md p-1 hover:bg-[var(--muted)]"><X size={15} /></button>
      </div>
      <p className="mt-3 text-xs leading-5 text-[var(--muted-foreground)]">{t(mode === 'conversation'
        ? 'Finish recording to send. Replies play aloud using the current chat model.'
        : 'Finish recording to insert text into your message.')}</p>
      <div className="my-4 flex flex-col items-center gap-2">
        <button type="button" aria-label={t(recording ? 'Stop recording' : 'Click to speak')}
          disabled={!recording && (working || busy)} onClick={() => recording ? recorder.stop() : void start()}
          className={`flex h-14 w-14 items-center justify-center rounded-full transition-colors disabled:opacity-40 ${recording ? 'bg-red-500/15 text-red-500 ring-4 ring-red-500/10' : 'bg-[var(--primary)] text-[var(--primary-foreground)]'}`}>
          {working ? <Loader2 size={23} className="animate-spin" /> : recording ? <Square size={20} className="fill-current" /> : <Mic size={23} />}
        </button>
        <span role="status" aria-live="polite" className="text-xs">{t(status)}</span>
      </div>
      {transcript && <p className="mb-3 max-h-20 overflow-y-auto break-words text-xs text-[var(--muted-foreground)]">{transcript}</p>}
      {(error || recorder.error) && <p role="alert" className="mb-3 text-xs leading-5 text-red-500">{error || recorder.error}</p>}
      <div className="flex items-center justify-between gap-2 border-t border-[var(--border)] pt-3 text-[10px] text-[var(--muted-foreground)]">
        <span className="min-w-0 break-words">{models ? `${models.stt.model || 'STT'} · ${models.tts.model || 'TTS'}` : t('Server speech · up to 120 seconds')}</span>
        {active && <button type="button" onClick={end} className="flex shrink-0 items-center gap-1 text-xs text-red-500"><PhoneOff size={13} />{t('End voice conversation')}</button>}
        {phase === 'playing' && <Volume2 size={14} className="shrink-0" />}
      </div>
    </section>, document.body)}
  </div>
}
