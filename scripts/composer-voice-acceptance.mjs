// Run inside the development container with its existing Next server. All
// provider calls and writes use a disposable native backend, never dev data.
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { chromium } from '../packages/browser-testing/node_modules/playwright/index.mjs'

const root = fileURLToPath(new URL('../', import.meta.url))
const temporary = await mkdtemp(path.join(os.tmpdir(), 'composer-voice-'))
const artifacts = path.join(root, 'dist/composer-voice-acceptance')
const web = process.env.VOICE_QA_WEB || 'http://127.0.0.1:3782'
let backend, browser, page, logs = '', sttCalls = 0, ttsCalls = 0, llmCalls = 0
let denySpeech = false
const sockets = new Set()
const clip = Buffer.alloc(44 + 32000)
clip.write('RIFF'); clip.writeUInt32LE(clip.length - 8, 4); clip.write('WAVEfmt ', 8)
clip.writeUInt32LE(16, 16); clip.writeUInt16LE(1, 20); clip.writeUInt16LE(1, 22)
clip.writeUInt32LE(16000, 24); clip.writeUInt32LE(32000, 28)
clip.writeUInt16LE(2, 32); clip.writeUInt16LE(16, 34); clip.write('data', 36); clip.writeUInt32LE(32000, 40)
const provider = createServer(async (req, res) => {
  try {
    const chunks = []
    for await (const chunk of req) chunks.push(chunk)
    const raw = Buffer.concat(chunks)
    if (req.url === '/v1/audio/transcriptions') {
      assert.equal(req.headers.authorization, 'Bearer synthetic-speech-key')
      assert.ok(raw.includes(Buffer.from('sensevoice-small-int8')))
      assert.ok(!raw.includes(Buffer.from('name="language"')))
      sttCalls++
      res.setHeader('content-type', 'application/json')
      res.end(JSON.stringify({ text: '语音回归测试' }))
    } else if (req.url === '/v1/audio/speech') {
      assert.equal(req.headers.authorization, 'Bearer synthetic-speech-key')
      const body = JSON.parse(raw)
      assert.equal(body.model, 'kokoro-v1.1-int8')
      assert.equal(body.voice, 'zf_001')
      assert.equal(body.response_format, 'wav')
      assert.ok(Array.from(body.input).length <= 4096)
      ttsCalls++
      if (denySpeech) { res.writeHead(401).end('synthetic-speech-key'); return }
      res.writeHead(200, { 'content-type': 'audio/wav' }).end(clip)
    } else if (req.url === '/v1/chat/completions') {
      const body = JSON.parse(raw)
      assert.equal(body.model, 'synthetic-chat')
      llmCalls++
      res.writeHead(200, { 'content-type': 'text/event-stream' })
      res.end(`data: ${JSON.stringify({ choices: [{ delta: { content: '这是服务器语音对话的测试回复。' } }] })}\n\ndata: ${JSON.stringify({ choices: [{ delta: {}, finish_reason: 'stop' }] })}\n\ndata: [DONE]\n\n`)
    } else res.writeHead(404).end()
  } catch (error) { res.writeHead(500).end(String(error)) }
}).listen(0, '127.0.0.1')
await once(provider, 'listening')
const availablePort = async () => {
  const server = createServer().listen(0, '127.0.0.1'); await once(server, 'listening')
  const port = server.address().port; await new Promise(resolve => server.close(resolve)); return port
}
const poll = async (predicate, message) => {
  for (let i = 0; i < 300; i++) {
    if (await predicate()) return
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(message)
}
try {
  const port = await availablePort(), origin = `http://127.0.0.1:${port}`
  const base = `http://127.0.0.1:${provider.address().port}/v1`
  backend = spawn(path.join(root, 'target/debug/deeptutor-server'), ['--home', temporary, 'serve', '--port', String(port)], {
    env: { ...process.env, DEEPTUTOR_AUTH_ENABLED: 'false', SPEECH_BASE_URL: base, SPEECH_API_KEY: 'synthetic-speech-key',
      SPEECH_STT_MODEL: 'sensevoice-small-int8', SPEECH_TTS_MODEL: 'kokoro-v1.1-int8', SPEECH_VOICE: 'zf_001' },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  for (const output of [backend.stdout, backend.stderr]) output.on('data', data => { logs = (logs + data).slice(-10000) })
  await poll(async () => { try { return (await fetch(origin + '/health')).ok } catch { return false } }, 'Native backend did not start')
  const json = async (url, body) => {
    const res = await fetch(origin + url, { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
    assert.equal(res.status, 200, await res.text())
  }
  const status = await (await fetch(origin + '/api/voice/status')).json()
  assert.deepEqual(status, { stt: { configured: true, model: 'sensevoice-small-int8' }, tts: { configured: true, model: 'kokoro-v1.1-int8' } })
  assert.ok(!JSON.stringify(status).includes('synthetic-speech-key'))
  const catalog = { version: 1, connections: [], services: {
    llm: { active_profile_id: 'chat', active_model_id: 'model', profiles: [{ id: 'chat', name: 'Synthetic chat', binding: 'custom', api_format: 'openai_chat', base_url: base,
      models: [{ id: 'model', model: 'synthetic-chat', api_format: 'openai_chat' }] }] },
  } }
  await json('/api/settings/catalog', { catalog })
  await json('/api/settings/ui', { language: 'zh', voice_autoplay: true })
  // Invalid personal selections must fail rather than fall back to the server.
  await json('/api/settings/catalog', { catalog: { ...catalog, services: { ...catalog.services, stt: { active_profile_id: 'missing', active_model_id: 'missing', profiles: [] } } } })
  assert.equal((await (await fetch(origin + '/api/voice/status')).json()).stt.configured, false)
  await json('/api/settings/catalog', { catalog })
  console.log('PASS server environment defaults, model names, secret redaction, explicit selection precedence')

  browser = await chromium.launch({ executablePath: chromium.executablePath(), headless: true, args: ['--no-sandbox', '--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream'] })
  const context = await browser.newContext({ viewport: { width: 1360, height: 900 }, permissions: ['microphone'] })
  await context.addInitScript(() => {
    window.__voiceTracks = []; window.__voiceMicCalls = 0
    const get = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices)
    navigator.mediaDevices.getUserMedia = async options => {
      window.__voiceMicCalls++
      const stream = await get(options); window.__voiceTracks.push(...stream.getTracks()); return stream
    }
  })
  await context.route('**/api/**', async route => {
    const url = new URL(route.request().url())
    try { await route.fulfill({ response: await route.fetch({ url: origin + url.pathname + url.search }) }) }
    catch { if (!page?.isClosed()) await route.abort().catch(() => {}) }
  })
  await context.routeWebSocket(/\/ws(?:\?|$)/, route => {
    const url = new URL(route.url())
    const socket = new WebSocket(origin.replace('http:', 'ws:') + url.pathname + url.search)
    sockets.add(socket)
    const pending = []
    route.onMessage(data => socket.readyState === WebSocket.OPEN ? socket.send(data) : pending.push(data))
    socket.onopen = () => { for (const data of pending) socket.send(data) }
    socket.onmessage = event => route.send(event.data)
    socket.onclose = () => { sockets.delete(socket); route.close() }
    socket.onerror = () => route.close()
    route.onClose(() => socket.close())
  })
  page = await context.newPage()
  page.setDefaultTimeout(30000)
  page.setDefaultNavigationTimeout(120000)
  page.on('pageerror', error => { logs += '\nBROWSER: ' + error.message })
  await page.goto(web + '/chat')
  await mkdir(artifacts, { recursive: true })
  assert.equal(await page.locator('a[href="/voice"]').count(), 0)
  const mic = page.getByRole('button', { name: '语音对话', exact: true })
  await mic.click()
  const panel = page.getByRole('region', { name: '语音对话', exact: true })
  await panel.waitFor()
  await page.screenshot({ path: path.join(artifacts, 'desktop-panel.png') })
  if (process.env.VOICE_QA_PHASE !== 'mobile') {
    // A missing model must be reported before asking for the microphone.
    const absent = route => route.fulfill({ json: { stt: { configured: false, message: 'Choose a speech-to-text model in Settings → Voice and set it as default before recording.' }, tts: { configured: false } } })
    await page.route('**/api/voice/status*', absent)
    await panel.getByRole('button', { name: '点击说话', exact: true }).click()
    await panel.getByRole('alert').waitFor()
    assert.equal(await page.evaluate(() => window.__voiceMicCalls), 0)
    await page.unroute('**/api/voice/status*', absent)
    await panel.getByRole('button', { name: '语音输入', exact: true }).click()
    const input = page.locator('textarea').first()
    await input.fill('保留草稿')
    await panel.getByRole('button', { name: '点击说话', exact: true }).click()
    await panel.getByRole('button', { name: '停止录音', exact: true }).waitFor()
    await page.waitForTimeout(350)
    await panel.getByRole('button', { name: '停止录音', exact: true }).click()
    await poll(async () => (await input.inputValue()).includes('语音回归测试'), 'Dictation did not reach composer')
    assert.equal(await input.inputValue(), '保留草稿 语音回归测试')
    assert.equal(await page.evaluate(() => window.__voiceTracks.every(track => track.readyState === 'ended')), true)
    console.log('PASS missing configuration preflight, real MediaRecorder, dictation appends to draft, microphone cleanup')

    await input.fill('')
    await panel.getByRole('button', { name: '语音对话', exact: true }).click()
    await panel.getByRole('button', { name: '点击说话', exact: true }).click()
    await panel.getByRole('button', { name: '停止录音', exact: true }).waitFor()
    await page.waitForTimeout(350)
    await panel.getByRole('button', { name: '停止录音', exact: true }).click()
    await poll(async () => ttsCalls >= 1, 'Voice reply was not synthesized')
    await panel.getByRole('button', { name: '点击说话', exact: true }).waitFor({ state: 'visible' })
    await poll(async () => await panel.getByRole('button', { name: '点击说话', exact: true }).isEnabled(), 'Playback did not finish')
    assert.equal(llmCalls, 1)
    assert.equal(ttsCalls, 1, 'Global autoplay must not duplicate voice playback')
    await page.getByText('这是服务器语音对话的测试回复。', { exact: true }).waitFor()
    await page.waitForTimeout(700)
    const popupBounds = await panel.boundingBox()
    const microphoneBounds = await page.getByRole('button', { name: '语音对话', exact: true }).first().boundingBox()
    assert.ok(Math.abs(popupBounds.y + popupBounds.height + 12 - microphoneBounds.y) < 2, 'Panel must follow the composer after the first message')
    await page.screenshot({ path: path.join(artifacts, 'desktop-reply.png') })
    console.log('PASS STT → selected chat model → Kokoro WAV playback, first draft binding, no duplicate autoplay')

    // Provider failures are actionable and do not expose secrets.
    denySpeech = true
    await panel.getByRole('button', { name: '点击说话', exact: true }).click()
    await panel.getByRole('button', { name: '停止录音', exact: true }).waitFor()
    await page.waitForTimeout(350)
    await panel.getByRole('button', { name: '停止录音', exact: true }).click()
    await panel.getByRole('alert').waitFor()
    assert.ok(!(await panel.innerText()).includes('synthetic-speech-key'))
    denySpeech = false
    await panel.getByRole('button', { name: '结束语音对话', exact: true }).click()
    assert.equal(await page.evaluate(() => window.__voiceTracks.every(track => track.readyState === 'ended')), true)
    await page.keyboard.press('Escape')
    await panel.waitFor({ state: 'hidden' })
    console.log('PASS redacted provider failure, end call and Escape')
  }

  if (process.env.VOICE_QA_PHASE !== 'desktop') {
    await page.setViewportSize({ width: 390, height: 844 })
    await page.waitForTimeout(750)
    if (!await panel.isVisible()) await mic.click()
    await panel.waitFor()
    let bounds = await panel.boundingBox()
    assert.ok(bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= 391 && bounds.y + bounds.height <= 845)
    await page.screenshot({ path: path.join(artifacts, 'mobile-panel.png') })
    await page.setViewportSize({ width: 667, height: 375 })
    await page.waitForTimeout(750)
    // The workspace shell may remount its content across the mobile breakpoint.
    if (!await panel.isVisible()) await mic.click()
    await panel.waitFor()
    bounds = await panel.boundingBox()
    assert.ok(bounds.x >= 0 && bounds.y >= 0 && bounds.x + bounds.width <= 668 && bounds.y + bounds.height <= 376)
    await page.screenshot({ path: path.join(artifacts, 'landscape-panel.png') })
    await page.setViewportSize({ width: 390, height: 844 })
    await page.waitForTimeout(750)
    if (!await panel.isVisible()) await mic.click()
    await panel.waitFor()
    await panel.getByRole('button', { name: '点击说话', exact: true }).click()
    await panel.getByRole('button', { name: '停止录音', exact: true }).waitFor()
    const before = sttCalls
    await panel.getByRole('button', { name: 'Close', exact: true }).or(panel.getByRole('button', { name: '关闭', exact: true })).click()
    await panel.waitFor({ state: 'hidden' })
    await page.waitForTimeout(300)
    assert.equal(sttCalls, before, 'Closing must discard recording')
    assert.equal(await page.evaluate(() => window.__voiceTracks.every(track => track.readyState === 'ended')), true)
    await page.goto(web + '/voice')
    await page.waitForURL('**/chat')
    console.log('PASS mobile/landscape fit, closing discards audio, old route redirect')
  }
} catch (error) {
  if (page && !page.isClosed()) {
    await mkdir(artifacts, { recursive: true })
    await page.screenshot({ path: path.join(artifacts, 'failure.png') }).catch(() => {})
    console.error((await page.locator('body').innerText()).slice(-6000))
  }
  console.error(logs)
  throw error
} finally {
  await browser?.close()
  for (const socket of sockets) socket.close()
  if (backend && backend.exitCode === null) { const stopped = once(backend, 'exit'); backend.kill('SIGTERM'); await stopped }
  await new Promise(resolve => provider.close(resolve))
  await rm(temporary, { recursive: true, force: true })
}
