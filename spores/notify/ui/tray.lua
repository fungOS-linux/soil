-- Notification tray and ringer UI.
--
-- Notifications are durable/deferrable; rings are active. The host owns focus
-- policy, history retention, app identity, delivery and the answer claim.
local ui = require("fungos.ui")
local action = require("fungos.action")

local M = { name = "dev.fungos.notify", version = "0.1.0" }

local function clip(value, limit)
  value = tostring(value or "")
  if #value <= limit then return value end
  return value:sub(1, limit - 1) .. "…"
end

local function item_rows(items, prefix)
  local rows = {}
  for index, item in ipairs(items or {}) do
    local id = prefix .. "-" .. index
    ui.push(rows, ui.column(id, { height = 58, gap = 2 }, {
      ui.row(id .. "-head", { height = 18 }, {
        ui.text(id .. "-app", clip(item.app_id or item.source or "app", 18), {
          size = 9,
          ink = "muted",
          flex_grow = 1,
        }),
        ui.text(id .. "-priority", tostring(item.priority or ""), {
          size = 9,
          ink = item.priority == "urgent" and "danger" or "muted",
          width = 72,
        }),
      }),
      ui.text(id .. "-title", clip(item.title or item.body, 54), {
        size = 12,
        height = 18,
        ink = item.read and "muted" or "text",
      }),
      ui.text(id .. "-body", clip(item.body or item.summary or "", 76), {
        size = 9,
        height = 14,
        ink = "muted",
      }),
    }))
  end
  return rows
end

local function section(title, id, items)
  local rows = { ui.text(id .. "-label", title, { size = 9, height = 16, ink = "muted" }) }
  for _, row in ipairs(item_rows(items, id)) do ui.push(rows, row) end
  if #(items or {}) == 0 then
    ui.push(rows, ui.text(id .. "-empty", "Nothing here.", { size = 10, height = 20, ink = "muted" }))
  end
  return ui.column(id, { flex_grow = 1, gap = 1 }, rows)
end

local function ringer(ctx, snapshot)
  local call = snapshot.call or {}
  local actions = {
    ui.text("ringer-source", clip(call.source or "unknown", 24), { size = 10, ink = "muted" }),
    ui.text("ringer-kind", tostring(call.kind or "call"), { size = 9, ink = "muted" }),
    { spacer = true },
    { id = "mute", label = "Mute", action = action.intent("ringer.mute", { id = call.id }) },
    { id = "decline", label = "Decline", danger = true, confirm = true,
      action = action.intent("ringer.decline", { id = call.id }) },
    { id = "accept", label = "Accept", variant = "primary",
      action = action.intent("ringer.accept", { id = call.id }) },
  }
  return ui.column("ringer-root", {
    width = (ctx.viewport and ctx.viewport.width) or 1086,
    height = (ctx.viewport and ctx.viewport.height) or 1569,
    gap = 12,
    padding = { top = 18, right = 54, bottom = 24, left = 54 },
  }, {
    ui.action_bar("ringer-actions", actions, { label = "Ringer actions" }),
    ui.column("ringer-body", { flex_grow = 1, gap = 10, padding = { top = 70 } }, {
      ui.text("ringer-title", call.title or "Incoming", { size = 34, height = 48, ink = "primary", wrap = true }),
      ui.text("ringer-detail", tostring(call.phase or "ringing"), { size = 12, height = 20, ink = "muted" }),
    }),
  })
end

function M.render(ctx)
  ctx = ctx or {}
  local snapshot = ctx.snapshot or {}
  if snapshot.call and snapshot.call.phase ~= "ended" then
    return ringer(ctx, snapshot)
  end
  local focus = snapshot.focus or {}
  local width = (ctx.viewport and ctx.viewport.width) or 1086
  local height = (ctx.viewport and ctx.viewport.height) or 1569
  local actions = {
    ui.text("notify-title", "NOTIFICATIONS", { size = 10, ink = "muted" }),
    ui.text("notify-focus", "Focus: " .. tostring(focus.mode or "off"), {
      size = 9,
      ink = focus.mode and focus.mode ~= "off" and "primary" or "muted",
    }),
    { spacer = true },
    { id = "focus", label = "Focus", action = action.intent("notify.focus") },
    { id = "cleanup", label = "Cleanup", danger = true, confirm = true,
      action = action.intent("notify.cleanup") },
    { id = "history", label = "Earlier", action = action.intent("notify.history") },
  }
  local root = ui.column("notify-root", {
    width = width,
    height = height,
    gap = 8,
    padding = { top = 12, right = 18, bottom = 16, left = 18 },
  }, {
    ui.action_bar("notify-actions", actions, { label = "Notification actions" }),
  })
  ui.push(root.children, ui.column("notify-main", { flex_grow = 1, gap = 16 }, {
    section("Now", "notify-now", snapshot.notifications),
    section("Held by Focus", "notify-held", snapshot.held),
    section("Earlier", "notify-history", snapshot.history),
  }))
  return root
end

return M
