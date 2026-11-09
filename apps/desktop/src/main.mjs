import { app, BrowserWindow, WebContentsView, Menu, ipcMain, shell, dialog, session, nativeTheme, net } from 'electron';
import { fork } from 'node:child_process';
import { watch } from 'node:fs';
import { mkdir, readFile, writeFile, rename, cp, rm } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { installBundle, bundleConfig, verifyManifest } from './bundles.mjs';
import { externalURL } from './config.mjs';
import { resolveLanguage, translate } from './i18n.mjs';
import { WINDOWS, agentMarker, edition, targetId } from './platform.mjs';
import { killTree } from './processes.mjs';
import { createUpgradeBackup, restoreUpgradeBackup } from './upgrade.mjs';
import { mediaPermissionPolicy } from './media-permissions.mjs';
import { checkGithubUpdate, downloadGithubUpdate } from './github-updates.mjs';
import { createErrorLogger } from './logging.mjs';

app.setName('Lorevia');
if (WINDOWS) app.setAppUserModelId('org.yanzhizhou.desktop');
const EDITION = edition();
const sourceDirectory = path.dirname(fileURLToPath(import.meta.url));
const shellURL = pathToFileURL(path.join(sourceDirectory, 'index.html')).href;
// Runtimes are large and machine-specific, so Windows keeps them out of the roaming profile.
// Keep the established data directory when upgrading the renamed app.
const userRoot = process.env.YANZHIZHOU_TEST_HOME || process.env.OPEN_DEEPTUTOR_TEST_HOME
  || path.join(WINDOWS ? process.env.LOCALAPPDATA ?? app.getPath('appData') : app.getPath('appData'), 'Open DeepTutor');
app.setPath('userData', userRoot);
const runtimeHome = path.join(userRoot, 'workspace');
const logPath = path.join(userRoot, 'logs/runtime.log');
const updateErrors = createErrorLogger({ directory: path.dirname(logPath) });
const activeFile = path.join(userRoot, 'active.json');
const transactionFile = path.join(userRoot, 'upgrade.json');
// Lorevia's own Settings page writes the interface language and theme here.
const interfaceFile = path.join(runtimeHome, 'data/user/settings/interface.json');
const seed = app.isPackaged ? path.join(process.resourcesPath, 'seed/runtime.json') : path.join(sourceDirectory, '../.local', targetId(), 'seed/runtime.json');
const publicKey = await readFile(path.join(sourceDirectory, '../assets/runtime-public.pem'), 'utf8');
// macOS draws the shell's own title bar under the traffic lights; Windows keeps its native frame and menu bar.
const HEADER_HEIGHT = WINDOWS ? 0 : 44;
const BACKGROUND = { light: '#ffffff', dark: '#161618' };
let window;
let view;
let worker;
let state = { phase: 'starting', step: 'opening' };
let ui = { language: 'en', theme: 'light' };
let interfaceWatcher;
let interfaceTimer;
let busy = false;
let quitting = false;
let exitAllowed = false;
let active;

const t = (key, values) => translate(ui.language, key, values);

async function writeJSON(file, data) {
  await mkdir(path.dirname(file), { recursive: true, mode: 0o700 });
  await writeFile(`${file}.tmp`, JSON.stringify(data, null, 2), { mode: 0o600 });
  await rename(`${file}.tmp`, file);
}

async function readJSON(file) {
  try { return JSON.parse(await readFile(file, 'utf8')); }
  catch (error) { if (error.code === 'ENOENT') return null; throw error; }
}

function coreVersion() {
  return active?.manifest.coreVersion ?? app.getVersion();
}

function updateOperation(source, id) {
  return updateErrors.operation({ source, app_version: app.getVersion(), current_version: coreVersion(), platform: process.platform, arch: process.arch }, id);
}

/** What the shell page shows, already in the user's language. */
function snapshot() {
  const failed = state.phase === 'error';
  return {
    phase: state.phase, theme: ui.theme, language: ui.language,
    title: failed ? t('errorTitle') : state.phase === 'stopped' ? t('stoppedTitle') : '',
    message: failed ? t('errorBody') : state.step ? t(state.step) : '',
    detail: failed ? state.message ?? '' : '',
    tagline: t('tagline'), retry: t('retry'), logs: t('logs'),
    edition: `${EDITION.full} v${coreVersion()}`,
  };
}

function publish() {
  if (window && !window.isDestroyed()) window.webContents.send('desktop:status', snapshot());
}

function status(next) {
  state = { ...state, ...next };
  publish();
  if (next.phase && next.phase !== 'ready') hideContent();
}

async function readInterface() {
  let saved;
  try { saved = await readJSON(interfaceFile) ?? {}; }
  catch { return ui; } // A half-written or hand-edited file keeps the current choice.
  const dark = saved.theme === 'dark' || (saved.theme === 'system' && nativeTheme.shouldUseDarkColors);
  return { language: resolveLanguage(saved.language), theme: dark ? 'dark' : 'light' };
}

async function refreshInterface() {
  const next = await readInterface();
  if (next.language === ui.language && next.theme === ui.theme) return;
  ui = next;
  buildMenu();
  window?.setBackgroundColor(BACKGROUND[ui.theme]);
  publish();
}

function watchInterface() {
  if (interfaceWatcher) return;
  try {
    // Settings are replaced atomically, so watch the folder rather than the file.
    interfaceWatcher = watch(path.dirname(interfaceFile), () => {
      clearTimeout(interfaceTimer);
      interfaceTimer = setTimeout(() => void refreshInterface(), 150);
    });
    interfaceWatcher.on('error', () => { interfaceWatcher?.close(); interfaceWatcher = undefined; });
  } catch { /* The folder appears after the core's first init; boot() tries again. */ }
}

function hideContent() {
  if (!view) return;
  window?.contentView.removeChildView(view);
  view.webContents.close();
  view = undefined;
}

function layout() {
  if (!view || !window) return;
  const bounds = window.getContentBounds();
  view.setBounds({ x: 0, y: HEADER_HEIGHT, width: bounds.width, height: Math.max(1, bounds.height - HEADER_HEIGHT) });
}

async function openContent(url) {
  hideContent();
  const origin = new URL(url).origin;
  view = new WebContentsView({ webPreferences: {
    preload: path.join(sourceDirectory, 'updates-preload.cjs'),
    contextIsolation: true, sandbox: true, nodeIntegration: false, webSecurity: true,
    partition: 'persist:deeptutor',
  } });
  const content = view.webContents;
  content.setWindowOpenHandler(({ url: target }) => {
    if (target.startsWith(origin + '/') || target.startsWith(`blob:${origin}/`)) {
      return { action: 'allow', overrideBrowserWindowOptions: {
        webPreferences: { contextIsolation: true, sandbox: true, nodeIntegration: false, webSecurity: true },
      } };
    }
    if (externalURL(target)) void shell.openExternal(target);
    return { action: 'deny' };
  });
  const restrict = (event, target) => {
    if (new URL(target).origin !== origin) {
      event.preventDefault();
      if (externalURL(target)) void shell.openExternal(target);
    }
  };
  content.on('will-navigate', restrict);
  content.on('will-redirect', restrict);
  content.on('will-attach-webview', event => event.preventDefault());
  content.on('render-process-gone', (_event, details) => {
    void updateOperation('renderer').log('renderer.failed', { reason: details.reason, exit_code: details.exitCode, runtime_id: active?.manifest.runtimeId });
    status({ phase: 'error', step: null, message: t('pageGone') });
  });
  window.contentView.addChildView(view);
  layout();
  await content.loadURL(url);
}

function trustedShell(event) {
  if (event.sender !== window?.webContents || event.senderFrame !== window.webContents.mainFrame
    || event.senderFrame.url !== shellURL) throw new Error('Untrusted desktop IPC caller');
}

async function stopWorker() {
  const previous = worker;
  worker = undefined;
  if (!previous || previous.exitCode !== null) return;
  await new Promise(resolve => {
    // Windows cannot signal the worker, so a stuck worker is ended with its services.
    const force = () => { if (WINDOWS) killTree(previous.pid); else previous.kill('SIGTERM'); };
    const timer = setTimeout(force, 12000);
    previous.once('exit', () => { clearTimeout(timer); resolve(); });
    if (previous.connected) previous.send({ type: 'stop' });
    else force();
  });
}

async function launchRuntime(installed, operation = updateOperation('runtime')) {
  await operation.log('runtime.starting', { runtime_id: installed.manifest.runtimeId, target_version: installed.manifest.coreVersion });
  const config = bundleConfig(installed.destination, runtimeHome, installed.manifest.runtimeId);
  const child = fork(path.join(sourceDirectory, 'supervisor.mjs'), [], {
    execPath: config.nodePath, stdio: ['ignore', 'ignore', 'pipe', 'ipc'],
    env: { ...process.env, ELECTRON_RUN_AS_NODE: '' },
  });
  worker = child;
  child.stderr.on('data', chunk => console.error(chunk.toString()));
  return new Promise((resolve, reject) => {
    let ready = false;
    child.on('message', next => {
      if (worker !== child || quitting) return;
      status(next);
      if (next.phase === 'ready' && !ready) { ready = true; resolve(next); }
      if (next.phase === 'error' && ready) void operation.log('runtime.failed', { error: new Error(next.message || 'Runtime failed'), phase: next.phase });
      if (next.phase === 'error' && !ready) reject(new Error(next.message));
    });
    child.once('error', error => {
      if (ready) void operation.log('runtime.failed', { error });
      else reject(error);
    });
    child.once('exit', code => {
      if (worker !== child || quitting) return;
      status({ phase: 'error', step: null, message: t('workerExited', { code }) });
      if (ready) void operation.log('runtime.failed', { error: new Error(state.message), exit_code: code });
      if (!ready) reject(new Error(state.message));
    });
    child.send({ type: 'start', config, logPath, cachePath: path.join(userRoot, 'cache', installed.manifest.runtimeId) });
  });
}

async function recoverInterruptedUpgrade() {
  const transaction = await readJSON(transactionFile);
  if (!transaction || transaction.phase !== 'testing') return;
  const operation = updateOperation('recovery', transaction.operationId);
  await operation.log('recovery.started', { target_version: transaction.candidate?.coreVersion, backup: transaction.backup });
  try {
    status({ phase: 'starting', step: 'recovering' });
    if (transaction.backup) {
      if (transaction.backupFormat === 'native-v1') {
        const previous = verifyManifest(transaction.previous, publicKey);
        const config = bundleConfig(path.join(userRoot, 'runtimes', previous.runtimeId), runtimeHome, previous.runtimeId);
        await restoreUpgradeBackup({ serverPath: config.serverPath, runtimeHome, backup: transaction.backup });
      } else if (transaction.backupFormat === undefined) {
        // Recover already-pending transactions written by an older shell.
        try { await rename(runtimeHome, `${runtimeHome}.interrupted-${Date.now()}`); }
        catch (error) { if (error.code !== 'ENOENT') throw error; }
        await cp(transaction.backup, runtimeHome, { recursive: true });
      } else throw new Error('Unsupported desktop recovery package format');
    }
    await writeJSON(activeFile, transaction.previous);
    await writeJSON(transactionFile, { ...transaction, phase: 'recovered' });
    await operation.log('recovery.completed');
  } catch (error) { await operation.log('recovery.failed', { error }); throw error; }
}

async function logStartupFailure(error, operation = updateOperation('startup')) {
  // Bundle extraction can fail before the supervisor creates the runtime log.
  console.error(error);
  await operation.log('startup.failed', { error });
}

async function boot() {
  if (busy || quitting) return;
  busy = true;
  const operation = updateOperation('startup');
  try {
    await operation.log('startup.stopping_previous');
    status({ phase: 'starting', step: 'starting' });
    await stopWorker();
    await recoverInterruptedUpgrade();
    await operation.log('startup.verifying_core');
    const saved = await readJSON(activeFile);
    const manifestFile = saved ? path.join(userRoot, 'runtimes', verifyManifest(saved, publicKey).runtimeId, 'installed.json') : seed;
    active = await installBundle(manifestFile, userRoot, publicKey, step => status({ step }));
    if (quitting) return;
    await operation.log('startup.starting_core', { runtime_id: active.manifest.runtimeId, current_version: active.manifest.coreVersion });
    const ready = await launchRuntime(active, operation);
    if (quitting) return;
    await writeJSON(activeFile, active.manifest);
    watchInterface();
    await refreshInterface();
    await operation.log('startup.opening_interface');
    await openContent(ready.url);
    await operation.log('runtime.running');
  } catch (error) {
    await logStartupFailure(error, operation);
    status({ phase: 'error', step: null, message: error.message });
    await stopWorker();
  } finally { busy = false; }
}

async function installUpdate(manifestFile, operation = updateOperation('local_update')) {
  if (busy || quitting || !active) return;
  busy = true;
  let transaction;
  let stopped = false;
  try {
    await operation.log('install.selecting_manifest');
    if (!manifestFile) {
      const choice = await dialog.showOpenDialog(window, { title: t('updateTitle'), properties: ['openFile'], filters: [{ name: t('updateFilter'), extensions: ['json'] }] });
      if (choice.canceled) { await operation.log('install.cancelled'); return; }
      manifestFile = choice.filePaths[0];
    }
    await operation.log('install.awaiting_confirmation');
    const confirmation = await dialog.showMessageBox(window, {
      type: 'question', buttons: [t('cancel'), t('updateConfirm')], defaultId: 0, cancelId: 0,
      message: t('updateMessage'), detail: t('updateDetail'),
    });
    if (confirmation.response !== 1 || quitting) { await operation.log('install.cancelled'); return; }
    await operation.log('install.verifying_core');
    const next = await installBundle(manifestFile, userRoot, publicKey, step => { void operation.log(`install.${step}`); });
    if (next.manifest.runtimeId === active.manifest.runtimeId) return;
    if (quitting) return;
    status({ phase: 'starting', step: 'updating' });
    stopped = true;
    await operation.log('install.stopping_core', { target_version: next.manifest.coreVersion, target_runtime_id: next.manifest.runtimeId });
    await stopWorker();
    const previousConfig = bundleConfig(active.destination, runtimeHome, active.manifest.runtimeId);
    await operation.log('install.creating_backup');
    const backup = await createUpgradeBackup({ serverPath: previousConfig.serverPath, runtimeHome, backupRoot: path.join(userRoot, 'backups') });
    await operation.log('install.backup_created', { backup });
    transaction = { phase: 'testing', previous: active.manifest, candidate: next.manifest, backup, backupFormat: 'native-v1', operationId: operation.id };
    await writeJSON(transactionFile, transaction);
    await operation.log('install.starting_new_core');
    const ready = await launchRuntime(next, operation);
    await operation.log('install.opening_interface');
    await openContent(ready.url);
    await operation.log('install.committing_version');
    await writeJSON(activeFile, next.manifest);
    await writeJSON(transactionFile, { ...transaction, phase: 'complete' });
    active = next;
    buildMenu();
    publish();
    await operation.log('runtime.running', { current_version: next.manifest.coreVersion });
  } catch (error) {
    await operation.log('install.failed', { error });
    if (!stopped) {
      await dialog.showMessageBox(window, { type: 'error', message: t('updateFailed'), detail: error.message });
      return;
    }
    await stopWorker();
    try { if (transaction) await recoverInterruptedUpgrade(); }
    catch (recoveryError) {
      await logStartupFailure(recoveryError, operation);
      status({ phase: 'error', step: null, message: recoveryError.message });
      return;
    }
    await dialog.showMessageBox(window, { type: 'error', message: t('updateFailed'), detail: error.message, buttons: [t('reopen')] });
    busy = false;
    await boot();
  } finally { busy = false; }
}

async function githubUpdate(operation = updateOperation('github_check')) {
  return checkGithubUpdate({ currentVersion: coreVersion(), fetchImpl: net.fetch, log: operation.log });
}

function updateSummary(update) {
  const { version, available, compatible, url } = update;
  return { version, available, compatible, url };
}

async function installGithubUpdate(expectedVersion) {
  if (busy || quitting || !active) throw new Error(t('updateBusy'));
  busy = true;
  const operation = updateOperation('github_update');
  let downloaded;
  try {
    const update = await githubUpdate(operation);
    await operation.log('update.selecting_version', { expected_version: expectedVersion, target_version: update.version });
    if (expectedVersion && expectedVersion !== update.version) throw new Error(t('updateChanged'));
    if (!update.available || !update.compatible) throw new Error(t('updateInstallerRequired'));
    downloaded = await downloadGithubUpdate({ update, root: path.join(userRoot, 'updates'), publicKey, fetchImpl: net.fetch, log: operation.log });
  } catch (error) {
    await operation.log('update.failed', { error });
    throw error;
  } finally { busy = false; }
  try { await installUpdate(downloaded.manifestFile, operation); }
  finally { await rm(downloaded.directory, { recursive: true, force: true }).catch(async error => { await operation.log('update_cleanup.failed', { error }); }); }
}

async function checkUpdateFromMenu() {
  try {
    const update = await githubUpdate();
    if (!update.available) {
      await dialog.showMessageBox(window, { message: t('updateCurrent'), buttons: ['OK'] });
      return;
    }
    const choice = await dialog.showMessageBox(window, {
      message: t('updateAvailable', { version: update.version }),
      detail: t(update.compatible ? 'githubUpdateDetail' : 'updateInstallerRequired'),
      buttons: [t('cancel'), t(update.compatible ? 'downloadUpdate' : 'openRelease')], defaultId: 0, cancelId: 0,
    });
    if (choice.response !== 1) return;
    if (update.compatible) await installGithubUpdate(update.version);
    else await shell.openExternal(update.url);
  } catch (error) {
    await dialog.showMessageBox(window, { type: 'error', message: t('githubUpdateFailed'), detail: error.message });
  }
}

function buildMenu() {
  const about = { label: t('about'), click: () => dialog.showMessageBox(window, {
    message: `${EDITION.full} v${coreVersion()}`,
    detail: t('aboutDetail', { core: active?.manifest.coreVersion ?? '—', runtime: active?.manifest.runtimeId ?? '—' }),
  }) };
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    { label: 'Lorevia', submenu: WINDOWS ? [about, { type: 'separator' }, { role: 'quit', label: t('quit') }] : [
      about,
      { type: 'separator' },
      { role: 'hide', label: t('hide') }, { role: 'hideOthers', label: t('hideOthers') }, { role: 'unhide', label: t('showAll') },
      { type: 'separator' },
      { role: 'quit', label: t('quit') },
    ] },
    { label: t('edit'), submenu: [
      { role: 'undo', label: t('undo') }, { role: 'redo', label: t('redo') }, { type: 'separator' },
      { role: 'cut', label: t('cut') }, { role: 'copy', label: t('copy') }, { role: 'paste', label: t('paste') },
      { role: 'selectAll', label: t('selectAll') },
    ] },
    { label: t('view'), submenu: [
      { label: t('reload'), accelerator: 'CmdOrCtrl+R', click: () => view?.webContents.reload() },
      { type: 'separator' },
      { label: t('actualSize'), accelerator: 'CmdOrCtrl+0', click: () => view?.webContents.setZoomLevel(0) },
      { label: t('zoomIn'), accelerator: 'CmdOrCtrl+Plus', click: () => view?.webContents.setZoomLevel(view.webContents.getZoomLevel() + 0.5) },
      { label: t('zoomOut'), accelerator: 'CmdOrCtrl+-', click: () => view?.webContents.setZoomLevel(view.webContents.getZoomLevel() - 0.5) },
      { type: 'separator' },
      { role: 'togglefullscreen', label: t('fullscreen') },
    ] },
    { label: t('core'), submenu: [
      { label: t('checkGithubUpdate'), click: () => void checkUpdateFromMenu() },
      { label: t('restartCore'), click: async () => {
        const choice = await dialog.showMessageBox(window, { message: t('restartConfirm'), buttons: [t('cancel'), t('restart')], defaultId: 0, cancelId: 0 });
        if (choice.response === 1) void boot();
      } },
      { label: t('installUpdate'), click: () => void installUpdate() }, { type: 'separator' },
      { label: t('openData'), click: () => void shell.openPath(runtimeHome) },
      { label: t('openLogs'), click: () => void shell.openPath(path.dirname(logPath)) },
    ] },
    { role: 'windowMenu', label: t('window'), submenu: WINDOWS ? [{ role: 'minimize', label: t('minimize') }] : [
      { role: 'minimize', label: t('minimize') }, { role: 'zoom', label: t('zoom') },
      { type: 'separator' }, { role: 'front', label: t('front') },
    ] },
  ]));
}

if (!app.requestSingleInstanceLock()) { app.quit(); }
else {
  app.on('second-instance', () => { window?.show(); window?.focus(); });
  app.on('before-quit', event => {
    if (exitAllowed) return;
    event.preventDefault();
    if (quitting) return;
    quitting = true;
    status({ phase: 'stopping', step: 'stopping' });
    void stopWorker().catch(error => logStartupFailure(error)).finally(async () => {
      await updateErrors.close();
      exitAllowed = true;
      app.quit();
    });
  });
  app.on('window-all-closed', () => app.quit());
  // Electron waits for ESM evaluation before emitting ready; do not await ready
  // at module scope or the packaged application deadlocks before its first window.
  app.whenReady().then(async () => {
  ui = await readInterface();
  nativeTheme.on('updated', () => void refreshInterface());
  const coreSession = session.fromPartition('persist:deeptutor');
  // The web UI reads this marker to name the client, e.g. "Lorevia for MacOS with Apple Silicon".
  coreSession.setUserAgent(`${coreSession.getUserAgent()} ${agentMarker(app.getVersion())}`);
  const media = mediaPermissionPolicy({
    currentContents: () => view?.webContents,
    currentUrl: () => state.url,
    ask: async () => {
      const result = await dialog.showMessageBox(window, { type: 'question', message: t('mediaPrompt'), buttons: [t('deny'), t('allow')], defaultId: 0, cancelId: 0 });
      return result.response === 1;
    },
  });
  coreSession.setPermissionCheckHandler(media.check);
  coreSession.setPermissionRequestHandler(media.request);
  window = new BrowserWindow({
    title: 'Lorevia', width: 1360, height: 920, minWidth: 900, minHeight: 620,
    backgroundColor: BACKGROUND[ui.theme],
    ...(WINDOWS ? {} : { titleBarStyle: 'hiddenInset', trafficLightPosition: { x: 18, y: 16 } }),
    webPreferences: { preload: path.join(sourceDirectory, 'preload.cjs'), contextIsolation: true, sandbox: true, nodeIntegration: false },
  });
  window.webContents.on('will-navigate', event => event.preventDefault());
  window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
  window.on('resize', layout);
  window.on('close', event => { if (!exitAllowed) { event.preventDefault(); app.quit(); } });
  for (const [channel, handler] of Object.entries({
    status: () => snapshot(),
    restart: () => { void boot(); },
    logs: async () => { await mkdir(path.dirname(logPath), { recursive: true }); await shell.openPath(path.dirname(logPath)); },
  })) ipcMain.handle(`desktop:${channel}`, (event, ...args) => { trustedShell(event); return handler(...args); });
  const trustedContent = event => {
    if (!view || event.sender !== view.webContents || event.senderFrame !== view.webContents.mainFrame
      || !state.url || new URL(event.senderFrame.url).origin !== new URL(state.url).origin) throw new Error('Untrusted update IPC caller');
  };
  ipcMain.handle('desktop:check-update', async event => { trustedContent(event); return updateSummary(await githubUpdate()); });
  ipcMain.handle('desktop:install-update', async (event, version) => {
    trustedContent(event);
    if (typeof version !== 'string' || !/^\d+\.\d+\.\d+$/.test(version)) throw new Error('Invalid update version');
    await installGithubUpdate(version);
  });
  buildMenu();
  watchInterface();
  await window.loadURL(shellURL);
  void boot();
  }).catch(async error => {
    await logStartupFailure(error);
    dialog.showErrorBox('Lorevia', error.message);
    exitAllowed = true;
    app.quit();
  });
}
