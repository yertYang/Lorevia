// No live model or credentials. Exercise the public turn API and restart the
// real backend against the same disposable home before reading its artifacts.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createServer } from "node:http";
import { mkdtemp, rm, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";

const home = await mkdtemp(path.join(tmpdir(), "lesson-qa-"));
const binary = path.resolve(
  process.env.NATIVE_TEST_BINARY ||
    "target/debug/deeptutor-server" +
      (process.platform === "win32" ? ".exe" : ""),
);
const lesson = {
  title: "能量守恒",
  code: `import {Scenes,Stage,Title,lessonLength} from './kit';
const scenes=[{voice:'s1',frames:90,content:<Stage><Title>Energy</Title></Stage>}];
export const LESSON={fps:30,width:1920,height:1080,durationInFrames:lessonLength(scenes)};
export const Lesson=()=> <Scenes scenes={scenes}/>;`,
  narration: [{ id: "s1", text: "Energy changes form." }],
};
// One second of 64x64 blue H.264 video (FFmpeg color source); plain text so CI does not need Git LFS.
const clip = Buffer.from(
  await readFile(
    new URL("./fixtures/video-qa.mp4.b64", import.meta.url),
    "utf8",
  ),
  "base64",
);
const wav = Buffer.alloc(44 + 16000);
wav.write("RIFF");
wav.writeUInt32LE(wav.length - 8, 4);
wav.write("WAVEfmt ", 8);
wav.writeUInt32LE(16, 16);
wav.writeUInt16LE(1, 20);
wav.writeUInt16LE(1, 22);
wav.writeUInt32LE(8000, 24);
wav.writeUInt32LE(16000, 28);
wav.writeUInt16LE(2, 32);
wav.writeUInt16LE(16, 34);
wav.write("data", 36);
wav.writeUInt32LE(16000, 40);
let attempt = 0,
  sceneCalls = 0,
  speechCalls = 0,
  backend,
  browser,
  tab,
  origin,
  mockOrigin,
  mockError,
  log = "",
  tool = "render_video_lesson",
  failJob = false,
  holdJob = false,
  jobSubmits = 0;
const mock = createServer(async (req, res) => {
  try {
    const chunks = [];
    for await (const b of req) chunks.push(b);
    const raw = Buffer.concat(chunks).toString();
    const json = (v) =>
      res
        .writeHead(200, { "content-type": "application/json" })
        .end(JSON.stringify(v));
    if (req.url === "/clip") {
      assert.equal(req.headers.authorization, undefined);
      return res.end(clip);
    }
    assert.equal(req.headers.authorization, "Bearer qa");
    if (req.url === "/v1/audio/speech") {
      speechCalls++;
      assert.equal(JSON.parse(raw).response_format, "wav");
      return res.writeHead(200, { "content-type": "audio/wav" }).end(wav);
    }
    if (req.url === "/v1/videos" && req.method === "POST") {
      jobSubmits++;
      assert.match(raw, /name="prompt"/);
      return json({ id: "job", status: "queued" });
    }
    if (req.url === "/v1/videos/job/content") return res.end(clip);
    if (req.url === "/v1/videos/job")
      return json({
        status: holdJob ? "in_progress" : failJob ? "failed" : "completed",
      });
    if (req.url === "/v1/videos/generations") return json({ request_id: "x" });
    if (req.url === "/v1/videos/x")
      return json({ status: "done", video: { url: mockOrigin + "/clip" } });
    if (req.url === "/v1/contents/generations/tasks") return json({ id: "v" });
    if (req.url === "/v1/contents/generations/tasks/v")
      return json({
        status: "succeeded",
        content: { video_url: mockOrigin + "/clip" },
      });
    if (req.url === "/v1/services/aigc/video-generation/video-synthesis") {
      assert.equal(req.headers["x-dashscope-async"], "enable");
      return json({ output: { task_id: "d" } });
    }
    if (req.url === "/v1/tasks/d")
      return json({
        output: { task_status: "SUCCEEDED", video_url: mockOrigin + "/clip" },
      });
    assert.equal(req.url, "/v1/chat/completions");
    const body = JSON.parse(raw);
    assert.ok(body.tools.some((t) => t.function.name === tool));
    const previous = body.messages.findLast((m) => m.role === "tool");
    const retry =
      tool === "render_video_lesson" &&
      previous &&
      !JSON.parse(previous.content).success;
    const call = !previous || retry;
    const args =
      tool === "videogen"
        ? { prompt: "A bouncing ball", duration: 4 }
        : sceneCalls++ === 0
          ? { ...lesson, code: "import 'node:fs';" + lesson.code }
          : lesson;
    const delta = call
      ? {
          tool_calls: [
            {
              index: 0,
              id: "call-" + ++attempt,
              type: "function",
              function: { name: tool, arguments: JSON.stringify(args) },
            },
          ],
        }
      : { content: "工具执行结束。" };
    res.writeHead(200, { "content-type": "text/event-stream" });
    for (const [d, f] of [
      [delta, null],
      [{}, call ? "tool_calls" : "stop"],
    ])
      res.write(
        "data: " +
          JSON.stringify({
            choices: [{ index: 0, delta: d, finish_reason: f }],
          }) +
          "\n\n",
      );
    res.end("data: [DONE]\n\n");
  } catch (e) {
    mockError = e;
    res.writeHead(500).end();
  }
});
async function listen(server) {
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  return server.address().port;
}
async function stop() {
  if (!backend || backend.exitCode !== null || backend.signalCode !== null)
    return;
  const done = once(backend, "exit");
  backend.kill();
  await done;
}
async function launch() {
  const socket = createServer();
  const port = await listen(socket);
  await new Promise((r) => socket.close(r));
  origin = "http://127.0.0.1:" + port;
  backend = spawn(binary, ["--home", home, "serve", "--port", String(port)], {
    env: {
      ...process.env,
      SPEECH_BASE_URL: "",
      DEEPTUTOR_AUTH_ENABLED: "false",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  for (const s of [backend.stdout, backend.stderr])
    s.on("data", (b) => {
      log = (log + b).slice(-12000);
    });
  for (let i = 0; i < 200; i++) {
    if (
      await fetch(origin + "/health").then(
        (r) => r.ok,
        () => false,
      )
    )
      return;
    await delay(100);
  }
  throw Error(log);
}
async function api(url, body, method = body ? "POST" : "GET") {
  const r = await fetch(origin + url, {
    method,
    headers: { "content-type": "application/json" },
    body: body ? JSON.stringify(body) : undefined,
  });
  const v = await r.json();
  assert.equal(r.status, 200, JSON.stringify(v));
  return v;
}
let workspace, scope;
async function turn(capability, content) {
  const receipt = await api("/api/turns" + scope, {
    capability,
    content,
    client_submission_id: crypto.randomUUID(),
  });
  for (let i = 0; i < 1200; i++) {
    const { events } = await api(
      "/api/turns/" + receipt.turn_id + "/events" + scope,
    );
    if (mockError) throw mockError;
    if (events.some((e) => e.type === "done")) {
      const results = events
        .filter((e) => e.type === "tool_result")
        .map((e) => e.metadata.tool_metadata);
      assert.ok(
        results.length,
        JSON.stringify(events.filter((e) => e.type === "error")),
      );
      return { receipt, results };
    }
    await delay(100);
  }
  throw Error(log);
}
let catalog;
function service(binding, model) {
  return {
    active_profile_id: "p",
    active_model_id: "m",
    profiles: [
      {
        id: "p",
        name: "QA",
        binding,
        base_url: mockOrigin + "/v1",
        api_key: "qa",
        api_format: "chat_completions",
        models: [{ id: "m", model }],
      },
    ],
  };
}
try {
  mockOrigin = "http://127.0.0.1:" + (await listen(mock));
  await launch();
  workspace = (
    await api("/api/settings/workspace/registrations", { name: "Video QA" })
  ).workspace.workspace_id;
  scope = "?workspace_id=" + workspace;
  catalog = {
    version: 1,
    connections: [],
    services: {
      llm: service("custom", "qa-chat"),
      videogen: service("custom", "qa-video"),
    },
  };
  const apply = () => api("/api/settings/catalog", { catalog }, "PUT");
  await apply();
  await api(
    "/api/settings/draft",
    { catalog, extensions: { tools: { enabled_tools: ["videogen"] } } },
    "PUT",
  );
  await api(
    "/api/settings/enabled-tools",
    { enabled_tools: ["videogen"] },
    "PUT",
  );
  await api("/api/settings/apply", {});
  await api("/api/settings/apply/registry", {
    kind: "default",
    service: "videogen",
    profile_id: "p",
    model_id: "m",
  });
  assert.ok(
    (await api("/api/tools")).enabled_optional_tools.includes("videogen"),
  );
  const silent = await turn("video_lesson", "做一个动画讲解");
  assert.equal(silent.results[0].error, "lesson_compile");
  const page = silent.results.at(-1).video_lesson;
  assert.equal(page.render.narration, "captions_only");
  assert.equal(page.render.checked_frames, 25);
  catalog.services.tts = service("custom", "qa-tts");
  await apply();
  const voiced = (await turn("chat", "做一个有旁白的动画讲解")).results.at(
    -1,
  ).video_lesson;
  assert.equal(voiced.render.narration, "voice_and_captions");
  assert.equal(speechCalls, 1);
  tool = "videogen";
  const short = await turn("chat", "生成 MP4 短片");
  const video = short.results[0].artifacts[0];
  assert.equal(video.mime_type, "video/mp4");
  failJob = true;
  const failed = await turn("chat", "生成短片");
  assert.equal(failed.results[0].success, false);
  assert.equal(failed.results[0].artifacts, undefined);
  failJob = false;
  assert.equal(failed.results[0].error, "video_poll");
  const errors = log
    .split("\n")
    .filter((l) => l.includes("video_tool_error"))
    .map((l) => JSON.parse(l));
  assert.ok(
    errors.some(
      (e) =>
        e.turn === failed.receipt.turn_id &&
        e.error_code === "video_poll" &&
        e.operation,
    ),
  );
  assert.ok(
    errors.every(
      (e) =>
        !JSON.stringify(e).includes("Bearer qa") &&
        !JSON.stringify(e).includes("bouncing ball"),
    ),
  );
  holdJob = true;
  const before = jobSubmits;
  const cancelled = await api("/api/turns" + scope, {
    capability: "chat",
    content: "生成可取消的短片",
    client_submission_id: crypto.randomUUID(),
  });
  for (let i = 0; i < 100 && jobSubmits === before; i++) await delay(100);
  assert.equal(jobSubmits, before + 1);
  const base = "/api/turns/" + cancelled.turn_id;
  await api(base + "/cancel" + scope, { command_id: crypto.randomUUID() });
  let cancelledEvents = [];
  for (let i = 0; i < 100; i++) {
    cancelledEvents = (await api(base + "/events" + scope)).events;
    if (cancelledEvents.some((e) => e.type === "done")) break;
    await delay(100);
  }
  assert.equal(
    cancelledEvents.find((e) => e.type === "done").metadata.status,
    "cancelled",
  );
  const invocation = cancelledEvents.find((e) => e.type === "tool_call")
    .metadata.invocation_id;
  assert.equal(
    (await fetch(origin + "/api/video/artifacts/" + invocation + scope)).status,
    404,
  );
  holdJob = false;
  for (const name of ["xai", "volcengine", "dashscope"]) {
    catalog.services.videogen = service(name, "qa-video");
    await apply();
    const result = await turn("chat", "生成短片");
    assert.equal(result.results[0].artifacts[0].size_bytes, clip.length);
  }
  await stop();
  await launch();
  for (const [entry, url] of [
    [silent, page.artifacts[0].url],
    [short, video.url],
  ]) {
    assert.ok(
      JSON.stringify(
        (await api("/api/sessions/" + entry.receipt.session_id + scope))
          .messages,
      ).includes(url),
    );
    assert.equal(
      (await fetch(origin + url.replace(workspace, "elsewhere"))).status,
      404,
    );
  }
  const url = origin + video.url;
  const ranged = await fetch(url, { headers: { Range: "bytes=4-7" } });
  assert.equal(ranged.status, 206);
  assert.equal(await ranged.text(), "ftyp");
  assert.equal(
    (await fetch(url, { headers: { Range: "bytes=999999999-" } })).status,
    416,
  );
  assert.equal(
    (await fetch(url, { method: "HEAD" })).headers.get("content-length"),
    String(clip.length),
  );
  assert.deepEqual(Buffer.from(await (await fetch(url)).arrayBuffer()), clip);
  const html = await fetch(origin + voiced.artifacts[0].url);
  assert.match(
    html.headers.get("content-security-policy"),
    /sandbox allow-scripts;/,
  );
  assert.ok((await html.text()).includes('id="player"'));
  if (process.env.VIDEO_QA_CHROME) {
    const { chromium } =
      await import("../packages/browser-testing/node_modules/playwright/index.mjs");
    browser = await chromium.launch({
      executablePath: process.env.VIDEO_QA_CHROME,
    });
    tab = await browser.newPage();
    await tab.goto(origin + voiced.artifacts[0].url);
    await tab.getByText("Energy", { exact: true }).waitFor();
    await tab.getByRole("button", { name: /play video/i }).click();
    await tab.getByText("Energy changes form.").waitFor();
    await tab.goto(url);
    await tab.waitForFunction(
      () => document.querySelector("video")?.duration > 0,
    );
    if (process.env.VIDEO_QA_WEB) {
      const web = process.env.VIDEO_QA_WEB;
      // Keep the development UI's data isolated: all its backend requests
      // go to this test's disposable account, including iframe navigation.
      tab.on("pageerror", (e) => console.error("Chat UI:", e.message));
      await tab.route("**/api/**", async (route) => {
        const u = new URL(route.request().url());
        try {
          await route.fulfill({
            response: await route.fetch({
              url: origin + u.pathname + u.search,
            }),
          });
        } catch {
          await route.abort().catch(() => {});
        }
      });
      await tab.goto(
        web +
          "/chat/" +
          silent.receipt.session_id +
          "?dt_workspace=" +
          workspace,
        { timeout: 120000, waitUntil: "domcontentloaded" },
      );
      await tab
        .frameLocator('iframe[sandbox="allow-scripts"]')
        .getByText("Energy", { exact: true })
        .waitFor({ timeout: 60000 });
      console.log(
        "PASS: reloaded course has an embedded playable Remotion frame",
      );
      await tab.goto(
        web +
          "/chat/" +
          short.receipt.session_id +
          "?dt_workspace=" +
          workspace,
        { waitUntil: "domcontentloaded" },
      );
      await tab.waitForFunction(
        () => document.querySelector("video")?.duration > 0,
        null,
        { timeout: 60000 },
      );
      console.log(
        "PASS: reloaded chat pages display the playable lesson and MP4 cards",
      );
    }
    console.log(
      "PASS: Chromium rendered the lesson, advanced to its caption, and decoded the MP4",
    );
  }
  await api(
    "/api/sessions/" + short.receipt.session_id + scope,
    undefined,
    "DELETE",
  );
  assert.equal((await fetch(url)).status, 404);
  console.log(
    "PASS: failed compile and repair, voiced and silent lessons, four video protocols, failed job, scoped playback, restart, deletion",
  );
} catch (error) {
  if (tab && !tab.isClosed())
    console.error(
      "Video QA page:",
      tab.url(),
      (
        await tab
          .locator("body")
          .innerText({ timeout: 1000 })
          .catch(() => "")
      ).slice(-1200),
    );
  throw error;
} finally {
  await browser?.close();
  await stop();
  mock.closeAllConnections();
  await new Promise((r) => mock.close(r));
  await rm(home, { recursive: true, force: true });
}
