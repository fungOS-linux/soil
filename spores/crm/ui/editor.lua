-- CRM snapshot UI. Pure projection over the host-provided snapshot; emits action
-- intents only. The host owns data access and namespaces.
local ui = require("fungos.ui")
local action = require("fungos.action")

local M = { name = "dev.fungos.crm", version = "0.1.0" }

local function clip(value, limit)
  value = tostring(value or "")
  if #value <= limit then return value end
  return value:sub(1, limit - 1) .. "…"
end

local function selected_account(snapshot)
  for _, account in ipairs(snapshot.accounts or {}) do
    if account.selected or account.id == snapshot.selected then return account end
  end
  return (snapshot.accounts or {})[1]
end

local function account_rail(ctx, snapshot)
  local children = {
    ui.text("crm-label", "ACCOUNTS", { size = 9, height = 16, ink = "muted" }),
  }
  for index, account in ipairs(snapshot.accounts or {}) do
    ui.push(children, ui.button("account-" .. index, clip(account.name or account.id, 28), {
      height = 28,
      size = 10,
      variant = account.selected and "primary" or nil,
      action = action.intent("crm.account", { id = account.id }),
    }))
  end
  if #(snapshot.accounts or {}) == 0 then
    ui.push(children, ui.text("crm-empty", "No accounts found.", {
      size = 10, height = 24, ink = "muted",
    }))
  end
  return ui.column("crm-rail", { width = 240, flex_shrink = 0, gap = 2 }, children)
end

local function detail(ctx, snapshot)
  local account = snapshot.account or selected_account(snapshot) or {}
  local children = {
    ui.text("crm-title", account.name or "CRM", { size = 22, height = 30, ink = "primary" }),
    ui.text("crm-subtitle", table.concat(account.domains or {}, ", "), {
      size = 10, height = 18, ink = "muted",
    }),
  }
  for index, contact in ipairs(account.contacts or {}) do
    ui.push(children, ui.text("crm-contact-" .. index,
      tostring(contact.name or contact.contact_token or "") .. " · " .. tostring(contact.role or ""), {
        size = 11, height = 22, wrap = true,
      }))
  end
  for index, event in ipairs(account.timeline or snapshot.events or {}) do
    ui.push(children, ui.text("crm-event-" .. index,
      tostring(event.kind or "event") .. " · " .. tostring(event.title or ""), {
        size = 10, height = 22, ink = "muted", wrap = true,
      }))
  end
  return ui.column("crm-detail", { flex_grow = 1, gap = 3 }, children)
end

function M.render(ctx, snapshot)
  return ui.row("crm-root", { flex_grow = 1, gap = 4 }, {
    account_rail(ctx, snapshot),
    detail(ctx, snapshot),
  })
end

return M
