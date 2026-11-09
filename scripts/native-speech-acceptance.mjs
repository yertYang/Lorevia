// Exercise the real Next → Rust → synthetic STT chain with a disposable home.
// Build Rust and web/.next-speech-qa first. WEB_QA=1 also exercises the recorder.
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createServer } from 'node:http'
import { mkdtemp, mkdir, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../', import.meta.url))
const temp = await mkdtemp(path.join(os.tmpdir(), 'native-speech-qa-'))
let backend, frontend, browser, page, logs = '', providerCalls = 0
const capture = child => {
  for (const stream of [child.stdout, child.stderr]) stream.on('data', data => { logs = (logs + data).slice(-16000) })
  return child
}
const stop = async child => {
  if (child && child.exitCode === null) { const done = once(child, 'exit'); child.kill('SIGTERM'); await done }
}
const port = async () => {
  const server = createServer().listen(0, '127.0.0.1')
  await once(server, 'listening')
  const value = server.address().port
  await new Promise(resolve => server.close(resolve))
  return value
}
const ready = async url => {
  const deadline = Date.now() + 30000
  while (Date.now() < deadline) {
    try { if ((await fetch(url, { signal: AbortSignal.timeout(2000) })).ok) return } catch {}
    await new Promise(resolve => setTimeout(resolve, 100))
  }
  throw new Error(`Service did not become ready: ${url}\n${logs}`)
}
// A short mono PCM clip lets the browser verify that the proxied bytes decode.
const sample = Buffer.alloc(44 + 1600)
sample.write('RIFF'); sample.writeUInt32LE(sample.length - 8, 4)
sample.write('WAVEfmt ', 8); sample.writeUInt32LE(16, 16); sample.writeUInt16LE(1, 20)
sample.writeUInt16LE(1, 22); sample.writeUInt32LE(8000, 24); sample.writeUInt32LE(16000, 28)
sample.writeUInt16LE(2, 32); sample.writeUInt16LE(16, 34); sample.write('data', 36)
sample.writeUInt32LE(sample.length - 44, 40)
const upstream = createServer(async (req, res) => {
  try {
    assert.equal(req.headers.authorization, 'Bearer synthetic-speech-key')
    const chunks = []
    for await (const chunk of req) chunks.push(chunk)
    if (req.url === '/v1/audio/speech') {
      assert.deepEqual(JSON.parse(Buffer.concat(chunks)), { model: 'tts-1', input: '试听', voice: 'nova', response_format: 'wav' })
      res.writeHead(200, { 'content-type': 'audio/wav' }).end(sample)
      return
    }
    assert.equal(req.url, '/v1/audio/transcriptions')
    const body = Buffer.concat(chunks).toString('latin1')
    assert.ok(body.includes('filename="recording.webm"'))
    assert.ok(body.includes('Content-Type: audio/webm\r\n'))
    assert.ok(!body.includes('codecs='))
    assert.ok(body.includes('synthetic-whisper'))
    providerCalls++
    res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ text: '语音回归测试' }))
  } catch (error) { res.writeHead(500).end(String(error)); }
}).listen(0, '127.0.0.1')
await once(upstream, 'listening')
const artifacts = path.join(root, 'dist/native-speech-acceptance')
try {
  const backendPort = await port(), webPort = await port()
  const origin = `http://127.0.0.1:${backendPort}`, web = `http://127.0.0.1:${webPort}`
  backend = capture(spawn(path.resolve(process.env.NATIVE_TEST_BINARY || path.join(root, 'target/release/deeptutor-server')),
    ['--home', path.join(temp, 'home'), 'serve', '--port', String(backendPort)], {
      env: { ...process.env, DEEPTUTOR_AUTH_ENABLED: 'false', NO_PROXY: '127.0.0.1,localhost' }, stdio: ['ignore', 'pipe', 'pipe'],
    }))
  await ready(origin + '/health')
  const standalone = path.join(root, 'web', process.env.WEB_QA_DIST || '.next-speech-qa', 'standalone')
  frontend = capture(spawn(process.execPath, [path.join(standalone, 'server.js')], {
    cwd: standalone, stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, PORT: String(webPort), HOSTNAME: '127.0.0.1', DEEPTUTOR_API_BASE_URL: origin, BACKEND_PORT: String(backendPort), NEXT_TELEMETRY_DISABLED: '1' },
  }))
  await ready(web + '/api/health')
  const json = async (url, body, method = 'PUT') => {
    const response = await fetch(web + url, { method, headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
    assert.equal(response.status, 200, await response.text())
  }
  const clip = () => {
    const form = new FormData()
    form.append('file', new Blob([Buffer.alloc(3 * 1024 * 1024, 7)], { type: 'audio/webm;codecs=opus' }), 'recording.webm')
    return form
  }
  // A fast release backend can reject the request while Next is still writing
  // its audio body. Repeat this race: the rewrite proxy used to replace 400
  // with an EPIPE-induced 500, although slower debug builds usually passed.
  for (let i = 0; i < 10; i++) {
    const absent = await fetch(web + '/api/voice/stt', { method: 'POST', body: clip() })
    assert.equal(absent.status, 400)
    assert.equal((await absent.json()).error_code, 'stt_not_configured')
  }
  assert.equal(providerCalls, 0)
  await json('/api/settings/ui', { language: 'zh' })
  const catalog = { version: 1, connections: [], services: {
    llm: { active_profile_id: 'llm', active_model_id: 'm', profiles: [{ id: 'llm', name: 'Synthetic model', binding: 'custom', base_url: 'http://127.0.0.1:9/v1', models: [{ id: 'm', model: 'synthetic-chat' }] }] },
    tts: { active_profile_id: 'playback', active_model_id: 'm', profiles: [{ id: 'playback', name: 'Synthetic TTS', binding: 'custom', base_url: `http://127.0.0.1:${upstream.address().port}/v1`, api_key: 'synthetic-speech-key', models: [{ id: 'm', model: 'tts-1', voice: 'nova', response_format: 'wav' }] }] },
    stt: { active_profile_id: 'speech', active_model_id: 'm', profiles: [{ id: 'speech', name: 'Synthetic speech', binding: 'custom', base_url: `http://127.0.0.1:${upstream.address().port}/v1`, api_key: 'synthetic-speech-key', models: [{ id: 'm', name: 'Synthetic Whisper', model: 'synthetic-whisper', language: 'zh' }] }] },
  } }
  if (process.env.WEB_QA === '1') {
    const { chromium } = await import('../packages/browser-testing/node_modules/playwright/index.mjs')
    browser = await chromium.launch({ channel: 'chrome', headless: true, args: ['--use-fake-device-for-media-stream', '--use-fake-ui-for-media-stream'] })
    const context = await browser.newContext({ permissions: ['microphone'], viewport: { width: 1360, height: 900 } })
    await context.addInitScript(() => {
      window.__speechTracks = []; window.__speechMicCalls = 0
      const original = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices)
      navigator.mediaDevices.getUserMedia = async options => {
        window.__speechMicCalls++
        const stream = await original(options); window.__speechTracks.push(...stream.getTracks()); return stream
      }
    })
    page = await context.newPage()
    page.setDefaultTimeout(30000)
    await page.goto(web + '/chat')
    await page.getByRole('button', { name: '语音对话', exact: true }).click()
    await page.getByRole('button', { name: '语音输入', exact: true }).click()
    await page.getByRole('button', { name: '点击说话', exact: true }).click()
    await page.getByRole('alert').filter({ hasText: '请先在“设置 → 语音”' }).waitFor()
    assert.equal(await page.evaluate(() => window.__speechMicCalls), 0)
    await mkdir(artifacts, { recursive: true })
    await page.screenshot({ path: path.join(artifacts, 'missing-configuration.png') })
  }
  await json('/api/settings/catalog', { catalog })
  for (const [path, body] of [
    ['/api/voice/tts', { text: '试听' }],
    ['/api/settings/voice/preview', { catalog, profile_id: 'playback', model_id: 'm', text: '试听' }],
  ]) {
    const synthesized = await fetch(web + path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
    assert.equal(synthesized.status, 200)
    assert.equal(synthesized.headers.get('content-type'), 'audio/wav')
    assert.deepEqual(Buffer.from(await synthesized.arrayBuffer()), sample)
  }
  const result = await fetch(web + '/api/voice/stt', { method: 'POST', body: clip() })
  assert.equal(result.status, 200, await result.clone().text())
  assert.equal((await result.json()).text, '语音回归测试')
  console.log('PASS: real Next upload forwarding, actionable missing-model response, >2 MiB multipart and provider transcript')
  if (page) {
    await page.getByRole('button', { name: '点击说话', exact: true }).click()
    const stopButton = page.getByRole('button', { name: '停止录音', exact: true })
    await stopButton.waitFor()
    await new Promise(resolve => setTimeout(resolve, 500))
    await stopButton.click()
    await page.waitForFunction(() => [...document.querySelectorAll('textarea')].some(input => input.value.includes('语音回归测试')))
    assert.equal(await page.evaluate(() => window.__speechTracks.every(track => track.readyState === 'ended')), true)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.getByRole('button', { name: '打开导航', exact: true }).waitFor()
    await page.mouse.move(380, 820)
    await new Promise(resolve => setTimeout(resolve, 500))
    await page.screenshot({ path: path.join(artifacts, 'transcript-mobile.png') })
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true)
    await page.goto(web + '/settings/voice')
    await page.getByRole('heading', { name: '播放', exact: true }).waitFor()
    const seconds = await page.evaluate(async () => {
      const response = await fetch('/api/voice/tts', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ text: '试听' }) })
      const context = new AudioContext()
      try { return (await context.decodeAudioData(await response.arrayBuffer())).duration }
      finally { await context.close() }
    })
    assert.ok(Math.abs(seconds - 0.1) < 0.001)
    await page.screenshot({ path: path.join(artifacts, 'voice-settings-mobile.png') })
    console.log('PASS: recorder preflight before microphone access, real MediaRecorder upload, composer transcript, microphone cleanup and native playback settings')
  }
} catch (error) {
  console.error(logs)
  throw error
} finally {
  await browser?.close()
  await stop(frontend); await stop(backend)
  await new Promise(resolve => upstream.close(resolve))
  await rm(temp, { recursive: true, force: true })
}
