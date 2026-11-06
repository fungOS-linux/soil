-- Contacts UI. Pure projection over a records-backed view; emits action intents only.
-- Contacts are personal. Sharing is explicit and only projects first name + token.
local ui = require("fungos.ui")
local action = require("fungos.action")

local M = { name = "dev.fungos.contacts", version = "0.1.0" }

local function clip(value, limit)
  value = tostring(value or "")
  if #value <= limit then return value end
  return value:sub(1, limit - 1) .. "…"
end

local function selected_contact(view)
  for _, contact in ipairs(view.contacts or {}) do
    if contact.selected or contact.id == view.selected then return contact end
  end
  return (view.contacts or {})[1]
end

local function rail(ctx, view)
  local children = {
    ui.text("contacts-label", "CONTACTS", { size = 9, height = 16, ink = "muted" }),
  }
  for index, contact in ipairs(view.contacts or {}) do
    ui.push(children, ui.button("contact-" .. index,
      clip(contact.name or contact.email or contact.id, 28), {
        height = 28,
        size = 10,
        variant = contact.selected and "primary" or nil,
        action = action.intent("contacts.select", { id = contact.id }),
      }))
  end
  if #(view.contacts or {}) == 0 then
    ui.push(children, ui.text("contacts-empty", "No contacts found.", {
      size = 10, height = 24, ink = "muted",
    }))
  end
  return ui.column("contacts-rail", { width = 240, flex_shrink = 0, gap = 2 }, children)
end

local function detail(ctx, view)
  local contact = view.contact or selected_contact(view) or {}
  local children = {
    ui.text("contacts-title", contact.name or "Contact", { size = 22, height = 30, ink = "primary" }),
    ui.text("contacts-email", contact.email or "", { size = 11, height = 20, ink = "muted" }),
    ui.text("contacts-phone", contact.phone or "", { size = 11, height = 20, ink = "muted" }),
    ui.text("contacts-title-line", contact.title or "", { size = 11, height = 20, ink = "muted" }),
  }
  if contact.contact_token and contact.contact_token ~= "" then
    ui.push(children, ui.text("contacts-token", "token " .. contact.contact_token, {
      size = 9, height = 18, ink = "muted",
    }))
  end
  if contact.shared then
    ui.push(children, ui.text("contacts-shared", "shared with CRM", {
      size = 10, height = 20, ink = "primary",
    }))
  end
  for index, share in ipairs(view.shared or {}) do
    if share.contact_token == contact.contact_token then
      ui.push(children, ui.button("contacts-unshare-" .. index,
        "Unshare from " .. tostring(share.account_id), {
          height = 28,
          size = 10,
          action = action.intent("contacts.unshare", {
            account_id = share.account_id,
            contact_id = contact.id,
          }),
        }))
    end
  end
  return ui.column("contacts-detail", { flex_grow = 1, gap = 3 }, children)
end

function M.render(ctx, view)
  return ui.row("contacts-root", { flex_grow = 1, gap = 4 }, {
    rail(ctx, view),
    detail(ctx, view),
  })
end

return M
