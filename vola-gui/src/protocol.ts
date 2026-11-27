// Wire types shared with `vola-core/src/protocol.rs`.
// Every frame is one JSON object: client messages carry `type` and fields; server messages
// carry `type` and a `data` payload when they have one.

export type ClientMessage =
  | { type: "status" }
  | { type: "sessions"; limit?: number }
  | { type: "messages"; session_id: number }
  | { type: "new_session" }
  | { type: "resume"; session_id: number }
  | { type: "chat"; text: string }
  | { type: "cancel" }
  | { type: "set_model"; model: string }
  | { type: "models" }
  | { type: "skills" }
  | { type: "skill"; name: string }
  | { type: "notes"; query?: string; limit?: number }
  | { type: "add_note"; content: string; pinned?: boolean; key?: string }
  | { type: "delete_note"; id: number }
  | { type: "compress" }
  | { type: "tools" }
  | { type: "stats" };

export interface ConfigView {
  home: string;
  provider_tag: string;
  provider_type: string;
  base_url: string;
  model: string;
  num_ctx: number;
  temperature: number;
  max_rounds: number;
  max_tool_errors: number;
  cmd_timeout_secs: number;
  budget_chars: number;
  db_path: string;
  skills_dir: string;
  assets_dir: string;
  scripts_dir: string;
  prompt_path: string;
  summarizer_path: string;
  shell: string;
  allow_dangerous: boolean;
  verbose: boolean;
}

export interface ReadyData {
  protocol: number;
  version: string;
  config: ConfigView;
  session_id: number | null;
  agent_error: string | null;
}

export interface SessionRow {
  id: number;
  title: string;
  model: string;
  updated_at: number;
  msg_count: number;
}

export interface NoteRow {
  id: number;
  key: string | null;
  content: string;
  tags: string;
  pinned: boolean;
  updated_at: number;
}

export interface SkillView {
  name: string;
  description: string;
  tags: string;
  path: string;
  use_count: number;
  updated_at: number;
}

export interface ToolView {
  name: string;
  description: string;
}

export interface Stats {
  sessions: number;
  messages: number;
  summaries: number;
  notes: number;
  skills: number;
}

export interface ModelInfo {
  id: string;
  owned_by: string;
  size: number | null;
}

export interface StoredMessage {
  id: number;
  role: string;
  content: string;
  tool_name: string | null;
  tool_calls: unknown[] | null;
  tool_call_id: string | null;
  created_at: number;
}

export interface SummaryRow {
  upto_message_id: number;
  text: string;
}

export interface TurnProgress {
  round: number;
  ctx_chars: number;
  budget_chars: number;
  num_ctx: number;
  prompt_tokens: number | null;
  completion_tokens: number | null;
  total_completion_tokens: number;
}

export interface TurnReport {
  text: string;
  rounds: number;
  tool_calls: number;
  elapsed_secs: number;
  tokens_per_sec: number | null;
  prompt_tokens: number | null;
  completion_tokens: number;
  compressed: boolean;
  ctx_chars: number;
  cancelled: boolean;
}

export interface StatusData {
  session_id: number | null;
  agent_error: string | null;
  model: string;
  num_ctx: number;
  budget_chars: number;
  ctx_chars: number | null;
  sessions: SessionRow[];
  skills: SkillView[];
  notes: NoteRow[];
  stats: Stats | null;
  tools: ToolView[];
}

export type ServerMessage =
  | { type: "ready"; data: ReadyData }
  | { type: "status"; data: StatusData }
  | { type: "sessions"; data: { sessions: SessionRow[] } }
  | {
      type: "messages";
      data: {
        session_id: number;
        messages: StoredMessage[];
        summary: SummaryRow | null;
      };
    }
  | { type: "session"; data: { session_id: number } }
  | { type: "model"; data: { model: string; num_ctx: number } }
  | { type: "models"; data: { models: ModelInfo[] } }
  | { type: "skills"; data: { skills: SkillView[] } }
  | { type: "skill"; data: { skill: SkillView; body: string } }
  | { type: "notes"; data: { notes: NoteRow[] } }
  | { type: "note"; data: { id: number } }
  | { type: "deleted"; data: { id: number; removed: boolean } }
  | { type: "compressed"; data: { compacted: boolean } }
  | { type: "tools"; data: { tools: ToolView[] } }
  | { type: "stats"; data: { stats: Stats } }
  | { type: "turn_started"; data: { session_id: number } }
  | { type: "progress"; data: TurnProgress }
  | { type: "tool_call"; data: { name: string; args: string } }
  | {
      type: "tool_result";
      data: { name: string; result: string; error: boolean };
    }
  | { type: "notice"; data: { text: string } }
  | { type: "turn_done"; data: TurnReport }
  | { type: "error"; data: { message: string } }
  | { type: "request_error"; data: { message: string } }
  | { type: "pong" };
