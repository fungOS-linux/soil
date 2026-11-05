-- Notes reader UI. The host supplies the snapshot; this view is a pure
-- projection and emits action intents only.
local ui = require("fungos.ui")
local action = require("fungos.action")

local M = { name = "dev.fungos.notes", version = "0.1.0" }

local function clip(value, limit)
  value = tostring(value or "")
  if #value <= limit then return value end
  return value:sub(1, limit - 1) .. "…"
end

local function selected_note(snapshot)
  for _, note in ipairs(snapshot.notes or {}) do
    if note.selected or note.path == snapshot.selected then return note end
  end
  return (snapshot.notes or {})[1]
end

local function note_list(ctx, snapshot)
  local children = {
    ui.text("notes-label", "NOTES", { size = 9, height = 16, ink = "muted" }),
  }
  for index, note in ipairs(snapshot.notes or {}) do
    ui.push(children, ui.button("note-" .. index, clip(note.title or note.path, 28), {
      height = 28,
      size = 10,
      variant = note.selected and "primary" or nil,
      action = action.intent("notes.open", { path = note.path }),
    }))
  end
  if #(snapshot.notes or {}) == 0 then
    ui.push(children, ui.text("notes-empty", "No notes found.", {
      size = 10,
      height = 24,
      ink = "muted",
    }))
  end
  return ui.column("notes-rail", { width = 240, flex_shrink = 0, gap = 2 }, children)
end

local function document(ctx, snapshot)
  local note = snapshot.document or {}
  local lines = {}
  for index, line in ipairs(note.lines or {}) do
    ui.push(lines, ui.text("note-line-" .. index, line.text or line, {
      size = line.level and (18 - line.level) or 11,
      height = line.level and 24 or 17,
      ink = line.level and "primary" or "text",
      wrap = true,
    }))
  end
  if #lines == 0 then
    ui.push(lines, ui.text("note-empty", "Select a note.", { size = 12, ink = "muted" }))
  end
  return ui.column("notes-document", { flex_grow = 1, gap = 3 }, {
    ui.text("note-title", note.title or "Notes", { size = 22, height = 30, ink = "primary" }),
    ui.text("note-path", note.path or "", { size = 9, height = 16, ink = "muted" }),
    ui.column("note-body", { flex_grow = 1, gap = 3 }, lines),
  })
end

local function links(ctx, snapshot)
  local children = {
    ui.text("backlinks-label", "BACKLINKS", { size = 9, height = 16, ink = "muted" }),
  }
  for index, link in ipairs(snapshot.backlinks or {}) do
    ui.push(children, ui.text("backlink-" .. index, clip(link.title or link.source or link, 30), {
      size = 10,
      height = 22,
      ink = "muted",
    }))
  end
  if #(snapshot.backlinks or {}) == 0 then
    ui.push(children, ui.text("backlinks-empty", "No backlinks indexed.", {
      size = 10,
      height = 22,
      ink = "muted",
    }))
  end
  return ui.column("notes-links", { width = 210, flex_shrink = 0, gap = 2 }, children)
end

function M.render(ctx)
  ctx = ctx or {}
  local snapshot = ctx.snapshot or {}
  local width = (ctx.viewport and ctx.viewport.width) or 1086
  local height = (ctx.viewport and ctx.viewport.height) or 1569
  local note = selected_note(snapshot)
  local actions = {
    ui.text("notes-title", "NOTES", { size = 10, ink = "muted" }),
    ui.text("notes-root", snapshot.root or "workspace", { size = 9, ink = "muted" }),
    { spacer = true },
    { id = "search", label = "Search", shortcut = "/", action = action.intent("notes.search") },
    { id = "present", label = "Present", disabled = note == nil,
      disabled_reason = note == nil and "Select a note" or nil,
      action = note and action.intent("notes.present", { path = note.path }) or nil },
    { id = "open", label = "Open", disabled = note == nil,
      disabled_reason = note == nil and "Select a note" or nil,
      action = note and action.intent("notes.open", { path = note.path }) or nil },
  }
  local root = ui.column("notes-root", {
    width = width,
    height = height,
    gap = 8,
    padding = { top = 12, right = 18, bottom = 16, left = 18 },
  }, {
    ui.action_bar("notes-actions", actions, { label = "Notes actions" }),
  })
  ui.push(root.children, ui.row("notes-main", { flex_grow = 1, gap = 16 }, {
    note_list(ctx, snapshot),
    document(ctx, snapshot),
    links(ctx, snapshot),
  }))
  return root
end

return M
