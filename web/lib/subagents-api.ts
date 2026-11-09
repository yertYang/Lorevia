import { apiFetch, apiUrl } from "@/lib/api";

/** One agent CLI backend's installability on this machine. */
export interface SubagentBackendInfo {
  kind: string;
  display_name: string;
  available: boolean;
  version: string;
  detail: string;
}

/** A connected subagent the user can consult in chat. */
export interface SubagentConnection {
  name: string;
  agent_kind: string;
  cwd: string;
  description?: string;
  created_at?: string;
  updated_at?: string | null;
}

export async function detectSubagents(): Promise<SubagentBackendInfo[]> {
  const res = await apiFetch(apiUrl("/api/subagents/detect"), {
    cache: "no-store",
  });
  if (!res.ok) throw new Error(`Detect failed: ${res.status}`);
  const data = (await res.json()) as { backends: SubagentBackendInfo[] };
  return data.backends ?? [];
}

export async function listSubagentConnections(): Promise<SubagentConnection[]> {
  const res = await apiFetch(apiUrl("/api/subagents/connections"), {
    cache: "no-store",
  });
  if (!res.ok) throw new Error(`Request failed: ${res.status}`);
  const data = (await res.json()) as { connections: SubagentConnection[] };
  return data.connections ?? [];
}

export async function connectSubagent(payload: {
  name: string;
  agent_kind: string;
  cwd?: string;
}): Promise<SubagentConnection> {
  const res = await apiFetch(apiUrl("/api/subagents/connections"), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload),
  });
  if (!res.ok) {
    const detail = await res
      .json()
      .then((d) => (d as { detail?: string }).detail)
      .catch(() => "");
    throw new Error(detail || `Connect failed: ${res.status}`);
  }
  return (await res.json()) as SubagentConnection;
}

export async function disconnectSubagent(name: string): Promise<void> {
  const res = await apiFetch(
    apiUrl(`/api/subagents/connections/${encodeURIComponent(name)}`),
    { method: "DELETE" },
  );
  if (!res.ok) throw new Error(`Disconnect failed: ${res.status}`);
}

/** One streamed line from the "message the agent directly" endpoint. */
export interface SubagentStreamLine {
  channel?: string;
  text?: string;
  merge_id?: string;
  done?: boolean;
  success?: boolean;
  session_id?: string;
}

/**
 * Send a message straight to a connected subagent and stream its run as
 * newline-delimited JSON. Resumes the same live session DeepTutor consults
 * (shared via the cross-turn registry), so the agent keeps context.
 */
export async function* streamSubagentMessage(
  name: string,
  body: { chat_session_id: string; message: string },
  signal?: AbortSignal,
): AsyncGenerator<SubagentStreamLine> {
  const res = await apiFetch(
    apiUrl(`/api/subagents/connections/${encodeURIComponent(name)}/message`),
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal,
    },
  );
  if (!res.ok || !res.body) throw new Error(`Message failed: ${res.status}`);
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  const flush = function* (chunk: string): Generator<SubagentStreamLine> {
    buf += chunk;
    let nl: number;
    while ((nl = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, nl).trim();
      buf = buf.slice(nl + 1);
      if (line) yield JSON.parse(line) as SubagentStreamLine;
    }
  };
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    yield* flush(decoder.decode(value, { stream: true }));
  }
  const tail = buf.trim();
  if (tail) yield JSON.parse(tail) as SubagentStreamLine;
}
