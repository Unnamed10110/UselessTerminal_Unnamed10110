// Stateful mock of the sessions/folders/snippets commands (browser-only dev backend, see mock.ts).
// Mirrors the Rust store closely enough to exercise the sidebar: validation, sortOrder renumbering,
// the §8.2 drop table, and a `sessions:changed` snapshot after every mutation.

import type { Folder, Session, SessionsSnapshot, Snippet } from "./types";

type Emit = (ev: string, payload: unknown) => void;
type At = "end" | { before: string } | { after: string };

let seq = 100;
const newId = () => (++seq).toString(16).padStart(32, "0");
const mkSession = (o: Partial<Session>): Session => ({
  id: newId(), name: "New Session", description: "", shellPath: "", arguments: "", workingDirectory: "", startingCommand: "",
  colorTag: "#00ff44", iconOverride: "", folderId: null, sortOrder: 0, themeBackground: "", themePreset: "", fontSize: 0,
  environment: "", integration: "auto", runAsAdmin: false, ...o,
});

let folders: Folder[] = [{ id: "f1", name: "Servers", sortOrder: 0 }, { id: "f2", name: "Local", sortOrder: 1 }, { id: "f3", name: "Empty folder", sortOrder: 2 }];
let sessions: Session[] = [
  mkSession({ id: "s1", name: "[SSH] build-server", description: "build.example.com as ci", shellPath: "ssh.exe", arguments: "build-server", colorTag: "#6be5ff", folderId: "f1" }),
  mkSession({ id: "s3", name: "[SSH] db-prod", shellPath: "ssh.exe", arguments: "db-prod", colorTag: "#ff003c", folderId: "f1", sortOrder: 1 }),
  mkSession({ id: "s4", name: "WSL Ubuntu", shellPath: "wsl.exe", arguments: "-d Ubuntu", colorTag: "#ff8800", folderId: "f2" }),
  mkSession({ id: "s5", name: "Dev shell", shellPath: "C:\\Program Files\\PowerShell\\7\\pwsh.exe", workingDirectory: "C:\\src", startingCommand: "git status", colorTag: "#7a5cff", folderId: "f2", sortOrder: 1, environment: "FOO=bar\nBAZ=1", fontSize: 16 }),
  mkSession({ id: "s2", name: "Project", shellPath: "pwsh.exe", workingDirectory: "C:\\src", startingCommand: "git status", sortOrder: 0 }),
  mkSession({ id: "s6", name: "Command Prompt", shellPath: "C:\\Windows\\System32\\cmd.exe", colorTag: "#ffff00", sortOrder: 1 }),
  mkSession({ id: "s7", name: "Scratch", description: "throw-away notes shell", shellPath: "pwsh.exe", colorTag: "#ff00ff", sortOrder: 2 }),
];
let snippets: Snippet[] = [
  { id: "n1", name: "Disk usage", command: "df -h", appendEnter: true, sortOrder: 0 },
  { id: "n2", name: "Big files", command: "find . -type f -size +100M \\\n  -exec ls -lh {} +", appendEnter: false, sortOrder: 1 },
];
let version = 1;
let banner: string | null = null;

const container = (st: Session[], folder: string | null) => st.filter((s) => s.folderId === folder).sort((a, b) => a.sortOrder - b.sortOrder);
const renumber = (folder: string | null) => container(sessions, folder).forEach((s, i) => (s.sortOrder = i));
const folderIds = () => [...folders].sort((a, b) => a.sortOrder - b.sortOrder).map((f) => f.id);
const setFolderOrder = (ids: string[]) => ids.forEach((id, i) => (folders.find((f) => f.id === id)!.sortOrder = i));
const treeOrder = () => [...folderIds().flatMap((f) => container(sessions, f)), ...container(sessions, null)];

/** Move `ids` (already in visual order) into `folder` at `at`, renumbering every touched container. */
function place(ids: string[], folder: string | null, at: At) {
  const touched = new Set<string | null>([folder]);
  for (const id of ids) touched.add(sessions.find((s) => s.id === id)!.folderId);
  const rest = container(sessions, folder).filter((s) => !ids.includes(s.id));
  const moved = ids.map((id) => sessions.find((s) => s.id === id)!);
  moved.forEach((s) => (s.folderId = folder));
  let pos = rest.length;
  if (at !== "end") {
    const i = rest.findIndex((s) => s.id === ("before" in at ? at.before : at.after));
    if (i >= 0) pos = "before" in at ? i : i + 1;
  }
  rest.splice(pos, 0, ...moved);
  rest.forEach((s, i) => (s.sortOrder = i));
  for (const f of touched) renumber(f);
}

function dropItems(ids: string[], folderId: string | null, target: { kind: string; id?: string }, half: string) {
  if (folderId) {
    const order = folderIds().filter((i) => i !== folderId);
    let pos = order.length;
    if (target.kind === "folder") {
      if (target.id === folderId) return;
      const p = order.indexOf(target.id!);
      if (p < 0) return;
      pos = p + (half === "bottom" ? 1 : 0);
    }
    order.splice(pos, 0, folderId);
    return setFolderOrder(order);
  }
  const dragged = treeOrder().map((s) => s.id).filter((id) => ids.includes(id));
  if (!dragged.length) return;
  if ((target.kind === "folder" || target.kind === "folderEdge") && folders.some((f) => f.id === target.id)) place(dragged, target.id!, "end");
  else if (target.kind === "session" && !dragged.includes(target.id!)) {
    const t = sessions.find((s) => s.id === target.id);
    if (t) place(dragged, t.folderId, half === "top" ? { before: t.id } : { after: t.id });
  } else if (target.kind === "root") place(dragged, null, "end");
}

const snapshot = (): SessionsSnapshot => ({
  folders: structuredClone(folders), sessions: structuredClone(sessions), snippets: structuredClone(snippets), banner, version,
});

/** Returns `undefined` for commands this mock does not own. Errors are thrown as strings, like Tauri rejections. */
export function mockSessions(cmd: string, a: Record<string, unknown>, emit: Emit): unknown {
  const changed = <T>(v: T): T => {
    version++;
    queueMicrotask(() => emit("sessions:changed", snapshot()));
    return v;
  };
  const check = (s: Session) => {
    if (!s.name.trim()) throw "Name is required.";
    if (!s.shellPath.trim()) throw "Shell path is required.";
    if (s.fontSize !== 0 && (s.fontSize < 8 || s.fontSize > 32)) s.fontSize = 0;
    if (!s.colorTag.trim()) s.colorTag = "#00ff44";
  };
  switch (cmd) {
    case "sessions_snapshot": return snapshot();
    case "sessions_search": {
      const q = String(a.query).trim().toLowerCase();
      return q ? treeOrder().filter((s) => [s.name, s.description, `${s.shellPath} ${s.arguments}`].some((t) => t.toLowerCase().includes(q))).sort((x, y) => x.sortOrder - y.sortOrder) : [];
    }
    case "sessions_clear_banner": banner = null; return undefined;
    case "session_add": {
      const s = mkSession(a.session as Partial<Session>);
      if (!s.id || sessions.some((x) => x.id === s.id)) s.id = newId();
      if (s.folderId && !folders.some((f) => f.id === s.folderId)) s.folderId = null;
      check(s);
      sessions.push(s);
      place([s.id], s.folderId, "end");
      return changed(structuredClone(s));
    }
    case "session_update": {
      const s = a.session as Session;
      const old = sessions.find((x) => x.id === s.id);
      if (!old) throw "Session not found.";
      check(s);
      const dest = s.folderId && folders.some((f) => f.id === s.folderId) ? s.folderId : null;
      Object.assign(old, s, { folderId: old.folderId, sortOrder: old.sortOrder });
      if (dest !== old.folderId) place([old.id], dest, "end");
      return changed(structuredClone(old));
    }
    case "session_delete": {
      const s = sessions.find((x) => x.id === a.id);
      if (s) { sessions = sessions.filter((x) => x !== s); renumber(s.folderId); }
      return changed(undefined);
    }
    case "session_duplicate": {
      const src = sessions.find((x) => x.id === a.id);
      if (!src) throw "Unknown session";
      const copy = { ...src, id: newId() };
      sessions.push(copy);
      place([copy.id], copy.folderId, { after: src.id });
      return changed(structuredClone(copy));
    }
    case "folder_add": {
      const f = { id: newId(), name: String(a.name).trim() || "New Folder", sortOrder: folders.length };
      folders.push(f);
      return changed(f);
    }
    case "folder_rename": {
      const f = folders.find((x) => x.id === a.id);
      if (f && String(a.name).trim()) f.name = String(a.name).trim();
      return changed(undefined);
    }
    case "folder_delete": {
      const kids = container(sessions, a.id as string).map((s) => s.id);
      place(kids, null, "end");
      folders = folders.filter((f) => f.id !== a.id);
      setFolderOrder(folderIds());
      return changed(undefined);
    }
    case "folder_move": {
      const ids = folderIds();
      const i = ids.indexOf(a.id as string);
      const j = i + (a.dir === "up" ? -1 : 1);
      if (i >= 0 && j >= 0 && j < ids.length) { [ids[i], ids[j]] = [ids[j], ids[i]]; setFolderOrder(ids); }
      return changed(undefined);
    }
    case "items_move": {
      const before = JSON.stringify([folders, sessions]);
      dropItems(a.sessionIds as string[], a.folderId as string | null, a.target as { kind: string; id?: string }, a.half as string);
      const moved = JSON.stringify([folders, sessions]) !== before;
      return moved ? changed(true) : false;
    }
    case "snippet_add": {
      const s = { id: newId(), name: "", command: "", appendEnter: true, ...(a.snippet as Partial<Snippet>), sortOrder: snippets.length };
      if (!s.name.trim() || !s.command.trim()) throw "Name and command are required.";
      snippets.push(s);
      return changed({ ...s });
    }
    case "snippet_update": {
      const s = a.snippet as Snippet;
      if (!s.name.trim() || !s.command.trim()) throw "Name and command are required.";
      const old = snippets.find((x) => x.id === s.id);
      if (old) Object.assign(old, s, { sortOrder: old.sortOrder });
      return changed({ ...s });
    }
    case "snippet_delete":
      snippets = snippets.filter((s) => s.id !== a.id);
      snippets.forEach((s, i) => (s.sortOrder = i));
      return changed(undefined);
    case "sessions_export": return "C:\\Users\\mock\\useless-terminal-sessions.json";
    case "sessions_import": {
      if (a.mode === "replace") { sessions = []; folders = []; }
      const s = mkSession({ name: "[Imported] Sample", shellPath: "pwsh.exe", colorTag: "#00e5ff" });
      sessions.push(s);
      place([s.id], null, "end");
      return changed({ added: 1, skipped: 1, message: "Imported 1 session(s)." });
    }
    case "import_wt": {
      const s = mkSession({ name: "[WT] Ubuntu", shellPath: "wsl.exe -d Ubuntu", colorTag: "#00e5ff" });
      sessions.push(s);
      place([s.id], null, "end");
      return changed({ added: 1, found: 2, message: "Imported 1 Windows Terminal profile(s)." });
    }
    case "import_ssh_config": {
      const hosts = ["web-1", "web-2"];
      for (const alias of hosts) {
        const s = mkSession({ name: `[SSH] ${alias}`, shellPath: "ssh.exe", arguments: alias, colorTag: "#6be5ff" });
        sessions.push(s);
        place([s.id], null, "end");
      }
      return changed({ added: hosts.length, message: `Imported ${hosts.length} SSH host(s) from ~/.ssh/config.` });
    }
    case "pick_file": return "C:\\Tools\\mock-shell.exe";
    case "pick_folder": return "C:\\Work\\project";
    case "icon_for": {
      // Tiny generated icon so the 20 px shell-icon slot is exercised; "" would show the fallback glyph.
      const stem = String(a.command).replace(/^"/, "").split(/[\\/ ]/).filter(Boolean).pop() ?? "?";
      const hue = [...stem].reduce((n, c) => n + c.charCodeAt(0), 0) * 37 % 360;
      const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" rx="6" fill="hsl(${hue} 60% 45%)"/><text x="16" y="22" font-size="18" text-anchor="middle" fill="#fff" font-family="Segoe UI">${stem[0]?.toUpperCase() ?? "?"}</text></svg>`;
      return `data:image/svg+xml,${encodeURIComponent(svg)}`;
    }
    default: return undefined;
  }
}
