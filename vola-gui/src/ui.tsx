// The GUI: a native TSX front end for the vola agent core.

import { Text, View, Scroll, effect, signal, type ScrollRef } from "nsh";
import { Input } from "nsh/base";
import { CoreServer } from "vola-gui-native";

import type { TurnProgress } from "./protocol";
import {
  activeTitle,
  addNote,
  agentError,
  busy,
  cancelTurn,
  compact,
  compressHistory,
  config,
  connect,
  connection,
  contextPercent,
  coreStatus,
  ctxChars,
  deleteNote,
  disconnect,
  draft,
  filteredSkills,
  formatTime,
  inspector,
  lastError,
  loadModels,
  messages,
  model,
  modelDraft,
  models,
  newSession,
  noteDraft,
  notePinned,
  notes,
  numCtx,
  openSkill,
  preview,
  refresh,
  resumeSession,
  selectedSkill,
  sendChat,
  serverVersion,
  sessionId,
  sessions,
  setModel,
  skillQuery,
  stats,
  summary,
  toast,
  toggleInspector,
  tools,
  turn,
  url,
  type InspectorTab,
  type ToolActivity,
  type UiMessage,
} from "./store";

const core = new CoreServer();

export function App() {
  return (
    <View class="flex-1 bg-background text-foreground">
      {connection() === "open" ? <Shell /> : <ConnectScreen />}
    </View>
  );
}

// ---------- connection screen ----------

function ConnectScreen() {
  const startCore = () => {
    const address = url().replace(/^wss?:\/\//, "");
    coreStatus.set(core.start("", address));
    setTimeout(() => connect(), 1200);
  };
  return (
    <View class="flex-1 items-center justify-center p-8">
      <View class="w-96 flex-col gap-4 rounded-2xl border border-border bg-card p-6">
        <View class="flex-row items-center gap-2">
          <View
            class="rounded-full bg-primary"
            style={{ width: 12, height: 12 }}
          />
          <Text class="text-2xl font-semibold">Vola</Text>
        </View>
        <Text class="text-sm text-muted-foreground">
          A native chat front end for the vola agent core. Start the core, then
          connect over WebSocket.
        </Text>
        <View class="flex-col gap-1">
          <Text class="text-xs font-medium text-muted-foreground">
            CORE ADDRESS
          </Text>
          <Input
            value={url()}
            onChange={(event) => url.set(event.value)}
            class="rounded-md border border-border bg-background px-3 py-2 text-sm"
          />
        </View>
        <View class="flex-row gap-2">
          <PressButton
            label={connection() === "connecting" ? "Connecting…" : "Connect"}
            primary
            onPress={() => connect()}
          />
          <PressButton label="Start core" onPress={startCore} />
        </View>
        <If when={coreStatus().length > 0}>
          <Text class="text-xs text-muted-foreground">{coreStatus()}</Text>
        </If>
        <If when={lastError() !== null}>
          <Text class="text-xs text-red-500">{lastError() ?? ""}</Text>
        </If>
        <Text class="text-xs text-muted-foreground">
          Or run this in a terminal: vola-agent serve --addr 127.0.0.1:8765
        </Text>
      </View>
    </View>
  );
}

// ---------- shell ----------

function Shell() {
  return (
    <View class="flex-1 flex-row">
      <Sidebar />
      <View class="flex-1 flex-col" style={{ minWidth: 0 }}>
        <Header />
        <Transcript />
        <Composer />
      </View>
      <If when={inspector() !== null}>
        <Inspector />
      </If>
    </View>
  );
}

function Sidebar() {
  return (
    <View class="w-64 border-r border-sidebar-border bg-sidebar p-3 gap-3">
      <View class="flex-row items-center gap-2 px-1">
        <View
          class="rounded-full bg-primary"
          style={{ width: 10, height: 10 }}
        />
        <Text class="text-lg font-semibold">vola</Text>
        <Text class="text-xs text-muted-foreground">
          {serverVersion() ? "core " + serverVersion() : ""}
        </Text>
      </View>

      <PressButton label="+ New chat" primary onPress={newSession} />

      <View class="flex-row items-center justify-between px-1">
        <Text class="text-xs font-medium text-muted-foreground">SESSIONS</Text>
        <Text class="text-xs text-muted-foreground">{sessions().length}</Text>
      </View>

      <Scroll
        class="flex-1"
        contentClass="flex-col gap-1"
        style={{ minHeight: 0 }}
      >
        <Each of={sessions()} key={(session) => session.id}>
          {(session) => (
            <View
              class="rounded-md px-2 py-2"
              classList={{ "bg-sidebar-accent": sessionId() === session().id }}
              onPress={() => resumeSession(session().id)}
            >
              <Text class="text-sm truncate">
                {session().title || "Untitled"}
              </Text>
              <Text class="text-xs text-muted-foreground">
                {session().msg_count} msg · {formatTime(session().updated_at)}
              </Text>
            </View>
          )}
        </Each>
        <If when={sessions().length === 0}>
          <Text class="text-xs text-muted-foreground px-2 py-1">
            No sessions yet.
          </Text>
        </If>
      </Scroll>

      <View class="flex-col gap-1 border-t border-sidebar-border pt-3">
        <SidebarButton label="Skills" tab="skills" />
        <SidebarButton label="Memory" tab="notes" />
        <SidebarButton label="Tools" tab="tools" />
        <SidebarButton label="Settings" tab="settings" />
      </View>
    </View>
  );
}

function SidebarButton(props: { label: string; tab: InspectorTab }) {
  return (
    <View
      class="rounded-md px-2 py-1.5 flex-row items-center justify-between"
      classList={{ "bg-sidebar-accent": inspector() === props.tab }}
      onPress={() => toggleInspector(props.tab)}
    >
      <Text class="text-sm">{props.label}</Text>
      <Text class="text-xs text-muted-foreground">›</Text>
    </View>
  );
}

function Header() {
  return (
    <View class="flex-row items-center gap-3 border-b border-border px-5 py-3">
      <View class="flex-1 flex-col" style={{ minWidth: 0 }}>
        <Text class="text-sm font-semibold truncate">{activeTitle()}</Text>
        <Text class="text-xs text-muted-foreground">
          {model()} · {compact(numCtx())} ctx
        </Text>
      </View>
      <View class="flex-col gap-1" style={{ width: 160 }}>
        <View class="flex-row justify-between">
          <Text class="text-xs text-muted-foreground">context</Text>
          <Text class="text-xs text-muted-foreground">{contextPercent()}%</Text>
        </View>
        <View class="rounded-full bg-secondary" style={{ height: 4 }}>
          <View
            class="rounded-full bg-primary"
            style={{ height: 4, width: `${contextPercent()}%` }}
          />
        </View>
      </View>
      <PressButton label="Compress" onPress={compressHistory} />
      <PressButton label="Refresh" onPress={refresh} />
    </View>
  );
}

// ---------- transcript ----------

function Transcript() {
  const scrollRef = signal<ScrollRef | null>(null);
  effect(() => {
    // Re-run when the transcript or the live turn grows.
    messages().length;
    turn()?.tools.length;
    turn()?.notices.length;
    const ref = scrollRef();
    if (ref) ref.scrollToEnd({ animated: true });
  });

  return (
    <Scroll
      ref={(node) => scrollRef.set(node)}
      class="flex-1"
      contentClass="flex-col gap-3 px-6 py-5"
      style={{ minHeight: 0 }}
    >
      <If when={summary() !== null}>
        <View class="rounded-lg border border-border bg-muted px-4 py-3 gap-1">
          <Text class="text-xs font-medium text-muted-foreground">
            History compacted into a summary through message #
            {summary()?.upto_message_id}
          </Text>
          <Text class="text-xs text-muted-foreground">
            {preview(summary()?.text ?? "", 500)}
          </Text>
        </View>
      </If>

      <If when={messages().length === 0 && turn() === null}>
        <EmptyState />
      </If>

      <Each of={messages()} key={(message) => message.key}>
        {(message) => <MessageRow message={message()} />}
      </Each>

      <If when={turn() !== null}>
        <LiveTurn />
      </If>
    </Scroll>
  );
}

function EmptyState() {
  const suggestions = [
    "List the files in this project",
    "Summarize what this repository does",
    "Run the tests and fix any failures",
    "Remember that I build with MSVC on Windows",
  ];
  return (
    <View class="flex-col gap-3 items-center py-10">
      <Text class="text-lg font-semibold">What should we build?</Text>
      <Text class="text-sm text-muted-foreground">
        The agent can run commands, read and write files, and curate its own
        skills.
      </Text>
      <View class="flex-col gap-2" style={{ width: 420 }}>
        <Each of={suggestions} key="index">
          {(suggestion) => (
            <View
              class="rounded-lg border border-border bg-card px-3 py-2"
              onPress={() => draft.set(suggestion())}
            >
              <Text class="text-sm">{suggestion()}</Text>
            </View>
          )}
        </Each>
      </View>
    </View>
  );
}

function MessageRow(props: { message: UiMessage }) {
  const message = props.message;

  if (message.role === "user") {
    return (
      <View
        class="rounded-2xl bg-primary px-4 py-2.5"
        style={{ alignSelf: "flex-end", maxWidth: "80%" }}
      >
        <Text class="text-sm text-primary-foreground">{message.content}</Text>
      </View>
    );
  }

  if (message.role === "assistant") {
    return (
      <View
        class="flex-col gap-1"
        style={{ alignSelf: "flex-start", maxWidth: "86%" }}
      >
        <View class="rounded-2xl border border-border bg-card px-4 py-3">
          <Text class="text-sm" style={{ lineHeight: 21 }}>
            {message.content}
          </Text>
        </View>
        <If when={message.meta !== undefined}>
          <Text class="text-xs text-muted-foreground">
            {message.meta ?? ""}
          </Text>
        </If>
      </View>
    );
  }

  if (message.role === "tool") {
    return (
      <View class="rounded-lg border border-border bg-muted px-3 py-2 gap-1">
        <View class="flex-row items-center gap-2">
          <Text class="text-xs font-mono text-muted-foreground">
            tool · {message.toolName ?? "?"}
          </Text>
          <If when={message.error === true}>
            <Text class="text-xs text-red-500">failed</Text>
          </If>
        </View>
        <If when={message.content.trim().length > 0}>
          <Text class="text-xs font-mono text-muted-foreground">
            {preview(message.content)}
          </Text>
        </If>
      </View>
    );
  }

  return (
    <Text
      class="text-xs text-muted-foreground text-center"
      classList={{ "text-red-500": message.error === true }}
    >
      {message.content}
    </Text>
  );
}

function LiveTurn() {
  const progress = (): TurnProgress | null => turn()?.progress ?? null;
  return (
    <View class="rounded-xl border border-border bg-card px-4 py-3 gap-2">
      <View class="flex-row items-center gap-2">
        <View
          class="rounded-full bg-amber-500"
          style={{ width: 8, height: 8 }}
        />
        <Text class="text-xs font-medium">
          {progress()?.round ? "round " + progress()!.round : "thinking…"}
        </Text>
        <Text class="text-xs text-muted-foreground">
          {progress()
            ? compact(progress()!.ctx_chars) +
              " ctx · " +
              (progress()!.prompt_tokens ?? "est") +
              " tok in · " +
              compact(progress()!.total_completion_tokens) +
              " out"
            : ""}
        </Text>
      </View>
      <Each of={turn()?.tools ?? []} key="index">
        {(tool) => <ToolCard tool={tool()} />}
      </Each>
      <Each of={turn()?.notices ?? []} key="index">
        {(notice) => <Text class="text-xs text-amber-600">{notice()}</Text>}
      </Each>
    </View>
  );
}

function ToolCard(props: { tool: ToolActivity }) {
  return (
    <View class="rounded-lg border border-border px-3 py-2 gap-1">
      <View class="flex-row items-center gap-2">
        <Text class="text-xs font-mono">{props.tool.name}</Text>
        <Text class="text-xs text-muted-foreground truncate">
          {props.tool.args}
        </Text>
      </View>
      <If when={props.tool.result !== null}>
        <Text
          class="text-xs font-mono"
          classList={{
            "text-red-500": props.tool.error,
            "text-muted-foreground": !props.tool.error,
          }}
        >
          {preview(props.tool.result ?? "")}
        </Text>
      </If>
    </View>
  );
}

// ---------- composer ----------

function Composer() {
  return (
    <View class="border-t border-border px-4 py-3 gap-2">
      <If when={agentError() !== null}>
        <Text class="text-xs text-red-500">{agentError() ?? ""}</Text>
      </If>
      <View class="flex-row items-end gap-2">
        <View class="flex-1" style={{ minWidth: 0 }}>
          <Input
            multiline
            value={draft()}
            placeholder="Ask vola…  (Enter sends, Shift+Enter adds a line)"
            submitBehavior="submit"
            onChange={(event) => draft.set(event.value)}
            onSubmit={() => sendChat()}
            class="rounded-lg border border-border bg-card px-3 py-2 text-sm"
            style={{ maxHeight: 180 }}
          />
        </View>
        {busy() ? (
          <PressButton label="Stop" danger onPress={cancelTurn} />
        ) : (
          <PressButton
            label="Send"
            primary
            disabled={draft().trim().length === 0}
            onPress={sendChat}
          />
        )}
      </View>
      <Text class="text-xs text-muted-foreground">{toast() ?? " "}</Text>
    </View>
  );
}

// ---------- inspector ----------

function Inspector() {
  return (
    <View
      class="w-80 border-l border-border bg-card p-4 gap-3"
      style={{ minWidth: 0 }}
    >
      <View class="flex-row items-center justify-between">
        <Text class="text-sm font-semibold">{tabLabel()}</Text>
        <PressButton label="Close" onPress={() => inspector.set(null)} />
      </View>
      <If when={inspector() === "skills"}>
        <SkillsPanel />
      </If>
      <If when={inspector() === "notes"}>
        <NotesPanel />
      </If>
      <If when={inspector() === "tools"}>
        <ToolsPanel />
      </If>
      <If when={inspector() === "settings"}>
        <SettingsPanel />
      </If>
    </View>
  );
}

function tabLabel(): string {
  switch (inspector()) {
    case "skills":
      return "Skills";
    case "notes":
      return "Memory";
    case "tools":
      return "Tools";
    case "settings":
      return "Settings";
    default:
      return "Inspector";
  }
}

function SkillsPanel() {
  return (
    <View class="flex-1 flex-col gap-2" style={{ minHeight: 0 }}>
      <Input
        value={skillQuery()}
        onChange={(event) => skillQuery.set(event.value)}
        placeholder="filter skills"
        class="rounded-md border border-border bg-background px-3 py-2 text-sm"
      />
      <Scroll
        class="flex-1"
        contentClass="flex-col gap-1"
        style={{ minHeight: 0 }}
      >
        <Each of={filteredSkills()} key={(skill) => skill.name}>
          {(skill) => (
            <View
              class="rounded-md border border-border px-3 py-2 gap-0.5"
              onPress={() => openSkill(skill().name)}
            >
              <Text class="text-sm font-medium">{skill().name}</Text>
              <Text class="text-xs text-muted-foreground">
                {skill().description}
              </Text>
            </View>
          )}
        </Each>
        <If when={filteredSkills().length === 0}>
          <Text class="text-xs text-muted-foreground">
            No skills yet. The agent saves them with the skill_save tool.
          </Text>
        </If>
      </Scroll>
      <If when={selectedSkill() !== null}>
        <View
          class="flex-col gap-1 border-t border-border pt-2"
          style={{ maxHeight: 260 }}
        >
          <Text class="text-sm font-semibold">
            {selectedSkill()?.skill.name ?? ""}
          </Text>
          <Scroll class="flex-1" contentClass="py-1" style={{ minHeight: 0 }}>
            <Text class="text-xs text-muted-foreground">
              {selectedSkill()?.body ?? ""}
            </Text>
          </Scroll>
        </View>
      </If>
    </View>
  );
}

function NotesPanel() {
  return (
    <View class="flex-1 flex-col gap-2" style={{ minHeight: 0 }}>
      <Input
        multiline
        value={noteDraft()}
        onChange={(event) => noteDraft.set(event.value)}
        placeholder="Save a fact for every turn…"
        class="rounded-md border border-border bg-background px-3 py-2 text-sm"
      />
      <View class="flex-row items-center gap-2">
        <PressButton
          label={notePinned() ? "Pinned" : "Not pinned"}
          onPress={() => notePinned.set(!notePinned())}
        />
        <PressButton
          label="Save note"
          primary
          disabled={noteDraft().trim().length === 0}
          onPress={addNote}
        />
      </View>
      <Scroll
        class="flex-1"
        contentClass="flex-col gap-1"
        style={{ minHeight: 0 }}
      >
        <Each of={notes()} key={(note) => note.id}>
          {(note) => (
            <View class="rounded-md border border-border px-3 py-2 gap-1">
              <Text class="text-xs">
                {note().pinned ? "📌 " : ""}
                {note().content}
              </Text>
              <View class="flex-row items-center justify-between">
                <Text class="text-xs text-muted-foreground">
                  {note().tags || "memory"} · {formatTime(note().updated_at)}
                </Text>
                <View onPress={() => deleteNote(note().id)}>
                  <Text class="text-xs text-red-500">delete</Text>
                </View>
              </View>
            </View>
          )}
        </Each>
        <If when={notes().length === 0}>
          <Text class="text-xs text-muted-foreground">No memories yet.</Text>
        </If>
      </Scroll>
    </View>
  );
}

function ToolsPanel() {
  return (
    <Scroll
      class="flex-1"
      contentClass="flex-col gap-1"
      style={{ minHeight: 0 }}
    >
      <Each of={tools()} key={(tool) => tool.name}>
        {(tool) => (
          <View class="rounded-md border border-border px-3 py-2 gap-0.5">
            <Text class="text-sm font-mono">{tool().name}</Text>
            <Text class="text-xs text-muted-foreground">
              {tool().description}
            </Text>
          </View>
        )}
      </Each>
      <If when={tools().length === 0}>
        <Text class="text-xs text-muted-foreground">No tools reported.</Text>
      </If>
    </Scroll>
  );
}

function SettingsPanel() {
  return (
    <Scroll
      class="flex-1"
      contentClass="flex-col gap-3"
      style={{ minHeight: 0 }}
    >
      <View class="flex-col gap-1">
        <Text class="text-xs font-medium text-muted-foreground">MODEL</Text>
        <Text class="text-sm">
          {model()} · {compact(numCtx())} ctx
        </Text>
        <Input
          value={modelDraft()}
          onChange={(event) => modelDraft.set(event.value)}
          class="rounded-md border border-border bg-background px-3 py-2 text-sm"
        />
        <View class="flex-row gap-2">
          <PressButton
            label="Use model"
            primary
            onPress={() => setModel(modelDraft())}
          />
          <PressButton label="List models" onPress={loadModels} />
        </View>
        <Each of={models()} key={(entry) => entry.id}>
          {(entry) => (
            <View
              onPress={() => {
                modelDraft.set(entry().id);
                setModel(entry().id);
              }}
            >
              <Text class="text-xs font-mono">{entry().id}</Text>
            </View>
          )}
        </Each>
      </View>

      <View class="flex-col gap-1">
        <Text class="text-xs font-medium text-muted-foreground">CORE</Text>
        <Text class="text-xs">ws {url()}</Text>
        <Text class="text-xs">
          provider {config()?.provider_tag ?? ""} ·{" "}
          {config()?.provider_type ?? ""}
        </Text>
        <Text class="text-xs">home {config()?.home ?? ""}</Text>
        <Text class="text-xs">database {config()?.db_path ?? ""}</Text>
        <Text class="text-xs">skills {config()?.skills_dir ?? ""}</Text>
        <Text class="text-xs">system {config()?.prompt_path ?? ""}</Text>
      </View>

      <View class="flex-col gap-1">
        <Text class="text-xs font-medium text-muted-foreground">STATS</Text>
        <Text class="text-xs">
          {stats()?.sessions ?? 0} sessions · {stats()?.messages ?? 0} messages
          · {stats()?.summaries ?? 0} summaries · {stats()?.notes ?? 0} notes ·{" "}
          {stats()?.skills ?? 0} skills
        </Text>
        <Text class="text-xs text-muted-foreground">
          context {ctxChars() === null ? "—" : compact(ctxChars() as number)}{" "}
          chars
        </Text>
      </View>

      <View class="flex-row gap-2">
        <PressButton label="Compress history" onPress={compressHistory} />
        <PressButton
          label="Stop core"
          onPress={() => {
            core.stop();
            coreStatus.set("core stopped");
            disconnect();
          }}
        />
        <PressButton label="Disconnect" danger onPress={disconnect} />
      </View>
    </Scroll>
  );
}

// ---------- primitives ----------

function PressButton(props: {
  label: string;
  onPress: () => void;
  primary?: boolean;
  danger?: boolean;
  disabled?: boolean;
}) {
  const primary = props.primary === true;
  const danger = props.danger === true;
  const disabled = props.disabled === true;
  return (
    <View
      class="rounded-md px-3 py-2 items-center justify-center"
      classList={{
        "bg-primary": primary,
        "bg-red-600": danger,
        "border border-border": !primary && !danger,
        "opacity-40": disabled,
      }}
      activeStyle={{ opacity: 0.8 }}
      disabled={disabled}
      onPress={props.onPress}
    >
      <Text
        class="text-sm font-medium"
        classList={{ "text-primary-foreground": primary, "text-white": danger }}
      >
        {props.label}
      </Text>
    </View>
  );
}
