// The GUI's state: one shared WebSocket to `vola-core`, signals the UI reads.

import { computed, signal } from "nsh";
import type {
  ClientMessage,
  ConfigView,
  ModelInfo,
  NoteRow,
  ServerMessage,
  SessionRow,
  SkillView,
  Stats,
  StoredMessage,
  SummaryRow,
  ToolView,
  TurnProgress,
  TurnReport,
} from "./protocol";

export type ConnectionState =
  "idle" | "connecting" | "open" | "closed" | "error";

export interface UiMessage {
  key: string;
  role: "user" | "assistant" | "tool" | "system";
  content: string;
  toolName?: string | null;
  meta?: string;
  error?: boolean;
  createdAt?: number;
}

export interface ToolActivity {
  name: string;
  args: string;
  result: string | null;
  error: boolean;
}

export interface TurnState {
  running: boolean;
  progress: TurnProgress | null;
  tools: ToolActivity[];
  notices: string[];
}

// ---------- connection ----------

export const url = signal("ws://127.0.0.1:8765");
export const connection = signal<ConnectionState>("idle");
export const lastError = signal<string | null>(null);
export const coreStatus = signal("");
export const toast = signal<string | null>(null);

// ---------- server snapshot ----------

export const serverVersion = signal("");
export const config = signal<ConfigView | null>(null);
export const sessions = signal<SessionRow[]>([]);
export const skills = signal<SkillView[]>([]);
export const notes = signal<NoteRow[]>([]);
export const tools = signal<ToolView[]>([]);
export const stats = signal<Stats | null>(null);
export const models = signal<ModelInfo[]>([]);
export const model = signal("");
export const numCtx = signal(0);
export const budgetChars = signal(0);
export const ctxChars = signal<number | null>(null);
export const agentError = signal<string | null>(null);
export const sessionId = signal<number | null>(null);

// ---------- conversation ----------

export const messages = signal<UiMessage[]>([]);
export const summary = signal<SummaryRow | null>(null);
export const turn = signal<TurnState | null>(null);
export const draft = signal("");
export const commandLog = signal<string[]>([]);

// ---------- inspector ----------

export type InspectorTab = "skills" | "notes" | "tools" | "settings";
export const inspector = signal<InspectorTab | null>(null);
export const selectedSkill = signal<{ skill: SkillView; body: string } | null>(
  null,
);
export const skillQuery = signal("");
export const noteDraft = signal("");
export const notePinned = signal(true);
export const modelDraft = signal("");

// ---------- derived ----------

export const busy = computed(() => turn()?.running === true);

export const contextPercent = computed(() => {
  const used = ctxChars();
  const budget = budgetChars();
  if (!used || !budget) return 0;
  return Math.min(100, Math.round((used / budget) * 100));
});

export const activeTitle = computed(() => {
  const id = sessionId();
  if (id === null) return "New chat";
  const row = sessions().find((s) => s.id === id);
  return row && row.title ? row.title : "Untitled session";
});

export const filteredSkills = computed(() => {
  const query = skillQuery().trim().toLowerCase();
  if (!query) return skills();
  return skills().filter((skill) =>
    (skill.name + " " + skill.description + " " + skill.tags)
      .toLowerCase()
      .includes(query),
  );
});

// ---------- socket ----------

let socket: WebSocket | null = null;
let toastTimer: ReturnType<typeof setTimeout> | null = null;
let sequence = 0;

function nextKey(prefix: string): string {
  sequence += 1;
  return prefix + "-" + sequence;
}

export function showToast(text: string) {
  toast.set(text);
  if (toastTimer !== null) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => {
    toast.set(null);
    toastTimer = null;
  }, 3500);
}

export function send(message: ClientMessage) {
  if (!socket || socket.readyState !== WebSocket.OPEN) {
    showToast("not connected to the core");
    return;
  }
  socket.send(JSON.stringify(message));
}

export function disconnect() {
  const old = socket;
  socket = null;
  if (old) {
    try {
      old.close();
    } catch {
      // already gone
    }
  }
  connection.set("closed");
}

export function connect(endpoint?: string) {
  if (endpoint) url.set(endpoint);
  disconnect();
  lastError.set(null);
  connection.set("connecting");

  let ws: WebSocket;
  try {
    ws = new WebSocket(url());
  } catch (error) {
    lastError.set(String(error));
    connection.set("error");
    return;
  }
  socket = ws;

  ws.onopen = () => {
    if (socket !== ws) return;
    connection.set("open");
    lastError.set(null);
    send({ type: "status" });
  };
  ws.onmessage = (event) => {
    if (socket !== ws) return;
    const raw = event.data;
    if (typeof raw === "string") handle(raw);
  };
  ws.onerror = () => {
    if (socket !== ws) return;
    lastError.set(
      "cannot reach " + url() + " — is `vola-agent serve` running?",
    );
  };
  ws.onclose = () => {
    if (socket !== ws) return;
    socket = null;
    connection.set("closed");
  };
}

// ---------- incoming ----------

function handle(raw: string) {
  let message: ServerMessage;
  try {
    message = JSON.parse(raw) as ServerMessage;
  } catch {
    return;
  }

  switch (message.type) {
    case "ready": {
      const data = message.data;
      serverVersion.set(data.version);
      config.set(data.config);
      model.set(data.config.model);
      numCtx.set(data.config.num_ctx);
      budgetChars.set(data.config.budget_chars);
      agentError.set(data.agent_error);
      modelDraft.set(data.config.model);
      break;
    }
    case "status": {
      const data = message.data;
      sessions.set(data.sessions);
      skills.set(data.skills);
      notes.set(data.notes);
      tools.set(data.tools);
      stats.set(data.stats);
      model.set(data.model);
      numCtx.set(data.num_ctx);
      budgetChars.set(data.budget_chars);
      ctxChars.set(data.ctx_chars);
      agentError.set(data.agent_error);
      sessionId.set(data.session_id);
      if (data.session_id !== null) {
        send({ type: "messages", session_id: data.session_id });
      }
      break;
    }
    case "messages": {
      const data = message.data;
      summary.set(data.summary);
      messages.set(
        data.messages
          .map(toUiMessage)
          .filter((item): item is UiMessage => item !== null),
      );
      break;
    }
    case "session": {
      sessionId.set(message.data.session_id);
      messages.set([]);
      summary.set(null);
      turn.set(null);
      send({ type: "messages", session_id: message.data.session_id });
      send({ type: "sessions" });
      send({ type: "stats" });
      break;
    }
    case "sessions":
      sessions.set(message.data.sessions);
      break;
    case "skills":
      skills.set(message.data.skills);
      break;
    case "skill":
      selectedSkill.set(message.data);
      inspector.set("skills");
      break;
    case "notes":
      notes.set(message.data.notes);
      break;
    case "note":
      showToast("saved memory #" + message.data.id);
      send({ type: "notes" });
      break;
    case "deleted":
      showToast(message.data.removed ? "memory deleted" : "memory not found");
      send({ type: "notes" });
      break;
    case "tools":
      tools.set(message.data.tools);
      break;
    case "stats":
      stats.set(message.data.stats);
      break;
    case "command_result": {
      const data = message.data;
      commandLog.update((list) => list.concat([data.text]));
      if (data.session_changed) showToast(data.text);
      // Commands can change the model, sessions, skills, notes or stats.
      send({ type: "status" });
      break;
    }
    case "session_deleted":
      showToast(message.data.removed ? "session deleted" : "session not found");
      send({ type: "sessions" });
      send({ type: "stats" });
      break;
    case "models":
      models.set(message.data.models);
      showToast(message.data.models.length + " models available");
      break;
    case "model":
      model.set(message.data.model);
      numCtx.set(message.data.num_ctx);
      modelDraft.set(message.data.model);
      showToast("model → " + message.data.model);
      break;
    case "compressed":
      showToast(
        message.data.compacted
          ? "history compacted into a summary"
          : "too few messages, nothing to compact",
      );
      send({ type: "status" });
      break;
    case "turn_started":
      sessionId.set(message.data.session_id);
      turn.update((current) =>
        current
          ? { ...current, running: true }
          : { running: true, progress: null, tools: [], notices: [] },
      );
      break;
    case "progress":
      turn.update((current) => {
        const base: TurnState = current ?? {
          running: true,
          progress: null,
          tools: [],
          notices: [],
        };
        return { ...base, running: true, progress: message.data };
      });
      break;
    case "tool_call":
      turn.update((current) => {
        const base: TurnState = current ?? {
          running: true,
          progress: null,
          tools: [],
          notices: [],
        };
        return {
          ...base,
          tools: base.tools.concat([
            {
              name: message.data.name,
              args: message.data.args,
              result: null,
              error: false,
            },
          ]),
        };
      });
      break;
    case "tool_result":
      turn.update((current) => {
        if (!current) return current;
        let index = -1;
        for (let i = current.tools.length - 1; i >= 0; i--) {
          if (
            current.tools[i].name === message.data.name &&
            current.tools[i].result === null
          ) {
            index = i;
            break;
          }
        }
        let toolList: ToolActivity[];
        if (index >= 0) {
          toolList = current.tools.slice();
          toolList[index] = {
            ...toolList[index],
            result: message.data.result,
            error: message.data.error,
          };
        } else {
          toolList = current.tools.concat([
            {
              name: message.data.name,
              args: "",
              result: message.data.result,
              error: message.data.error,
            },
          ]);
        }
        return { ...current, tools: toolList };
      });
      break;
    case "notice":
      showToast(message.data.text);
      turn.update((current) =>
        current
          ? { ...current, notices: current.notices.concat([message.data.text]) }
          : current,
      );
      break;
    case "turn_done":
      finishTurn(message.data);
      break;
    case "error":
      if (turn()?.running) {
        turn.set(null);
        messages.update((list) =>
          list.concat([
            {
              key: nextKey("error"),
              role: "system",
              content: message.data.message,
              error: true,
            },
          ]),
        );
      } else {
        showToast(message.data.message);
      }
      break;
    case "request_error":
      showToast(message.data.message);
      break;
    case "pong":
      break;
  }
}

function finishTurn(report: TurnReport) {
  const current = turn();
  turn.set(null);

  const additions: UiMessage[] = [];
  if (current) {
    for (const tool of current.tools) {
      additions.push({
        key: nextKey("tool"),
        role: "tool",
        content: tool.result ?? "",
        toolName: tool.name,
        error: tool.error,
      });
    }
  }

  if (report.cancelled) {
    additions.push({
      key: nextKey("sys"),
      role: "system",
      content: "Turn cancelled after " + report.elapsed_secs.toFixed(1) + "s.",
    });
  } else if (report.text.trim()) {
    additions.push({
      key: nextKey("assistant"),
      role: "assistant",
      content: report.text,
      meta: reportMeta(report),
    });
  } else {
    additions.push({
      key: nextKey("sys"),
      role: "system",
      content: "The model produced no output.",
    });
  }

  messages.update((list) => list.concat(additions));
  ctxChars.set(report.ctx_chars);
  send({ type: "sessions" });
  send({ type: "stats" });
}

function reportMeta(report: TurnReport): string {
  const speed = report.tokens_per_sec
    ? " · " + report.tokens_per_sec.toFixed(1) + " tok/s"
    : "";
  const compacted = report.compressed ? " · compacted" : "";
  return (
    report.rounds +
    " rounds · " +
    report.tool_calls +
    " tools · " +
    report.elapsed_secs.toFixed(1) +
    "s" +
    speed +
    compacted
  );
}

function toUiMessage(message: StoredMessage): UiMessage | null {
  if (
    message.role === "assistant" &&
    !message.content.trim() &&
    (message.tool_calls?.length ?? 0) > 0
  ) {
    return null;
  }
  const role: UiMessage["role"] =
    message.role === "user" ||
    message.role === "assistant" ||
    message.role === "tool"
      ? message.role
      : "system";
  return {
    key: "db-" + message.id,
    role,
    content: message.content,
    toolName: message.tool_name,
    createdAt: message.created_at,
  };
}

// ---------- actions ----------

export function sendChat() {
  const text = draft().trim();
  if (!text || busy()) return;
  draft.set("");
  if (text.startsWith("/")) {
    send({ type: "command", line: text });
    return;
  }
  commandLog.set([]);
  messages.update((list) =>
    list.concat([{ key: nextKey("user"), role: "user", content: text }]),
  );
  turn.set({ running: true, progress: null, tools: [], notices: [] });
  send({ type: "chat", text });
}

export function sendCommand(line: string) {
  const text = line.trim();
  if (text) send({ type: "command", line: text });
}

export function deleteSession(id: number) {
  send({ type: "delete_session", session_id: id });
}

export function cancelTurn() {
  send({ type: "cancel" });
}

export function newSession() {
  turn.set(null);
  messages.set([]);
  summary.set(null);
  send({ type: "new_session" });
}

export function resumeSession(id: number) {
  if (id === sessionId()) return;
  turn.set(null);
  send({ type: "resume", session_id: id });
}

export function refresh() {
  send({ type: "status" });
}

export function setModel(name: string) {
  const trimmed = name.trim();
  if (trimmed) send({ type: "set_model", model: trimmed });
}

export function loadModels() {
  send({ type: "models" });
}

export function addNote() {
  const text = noteDraft().trim();
  if (!text) return;
  send({ type: "add_note", content: text, pinned: notePinned() });
  noteDraft.set("");
}

export function deleteNote(id: number) {
  send({ type: "delete_note", id });
}

export function compressHistory() {
  send({ type: "compress" });
}

export function openSkill(name: string) {
  send({ type: "skill", name });
}

export function toggleInspector(tab: InspectorTab) {
  inspector.set(inspector() === tab ? null : tab);
  if (tab === "skills") send({ type: "skills" });
  if (tab === "notes") send({ type: "notes" });
  if (tab === "tools") send({ type: "tools" });
}

// ---------- formatting ----------

export function formatTime(seconds: number): string {
  if (!seconds) return "";
  return new Date(seconds * 1000).toLocaleString();
}

export function compact(value: number): string {
  if (value >= 1_000_000) return (value / 1_000_000).toFixed(1) + "M";
  if (value >= 1000) return (value / 1000).toFixed(1) + "k";
  return String(value);
}

export function preview(text: string, limit = 1200): string {
  if (text.length <= limit) return text;
  return (
    text.slice(0, limit) + "\n… (" + (text.length - limit) + " more characters)"
  );
}
