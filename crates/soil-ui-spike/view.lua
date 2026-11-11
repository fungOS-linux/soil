-- Composition only. Colours, sizes and radii come from theme.lua; components
-- are files that paint themselves.
package.path = "./?.lua;" .. package.path

local lib = dofile("ui.lua")
local theme = dofile("theme.lua")
local Button = require("components.button")
local NavRow = require("components.nav_row")
local Paragraph = require("components.paragraph")

local M = {}

function M.render(host, layout, state)
  local ui = lib.new(host, theme)
  local t = theme

  ui.rect({ x = 0, y = 0, w = layout.width, h = layout.height }, t.colour.background)

  local side = { x = 8, y = 8, w = layout.sidebar.width - 16, h = layout.height - 16 }
  ui.rect(side, t.colour.surface, t.radius.lg)

  local head = ui.box(side, t.space.lg)
  ui.text("SHELLS", { x = head.x, y = head.y + t.space.sm },
    { size = t.font.size.micro, colour = t.colour.faint })

  local y = 56
  ui.scope("nav", function()
    for _, name in ipairs(state.targets) do
      NavRow.render(ui, {
        key = name, label = name, selected = name == state.selected,
        hovered_id = state.hover, focused_id = state.focus,
        rect = { x = side.x + 8, y = y, w = side.w - 16, h = 30, cx = side.x + 8 + (side.w - 16) / 2, cy = y + 15 },
      })
      y = y + 36
    end
  end)

  local cx = layout.content.x + t.space.xl
  local content_width = layout.width - layout.content.x - t.space.xl * 2
  Paragraph.render(ui, {
    text = "This session is attached to a local shell. Keys go to the pty, the pty sends frames back, and the view repaints from the snapshot it is given.",
    x = cx, y = 84, width = content_width,
    colour = t.colour.faint, size = t.font.size.small,
  })
  ui.text(state.selected, { x = cx, y = 40 }, { size = t.font.size.title })
  ui.text("zsh · 80×24 · attached", { x = cx, y = 60 },
    { size = t.font.size.small, colour = t.colour.faint })

  Button.render(ui, { key = "new", label = "new session", action = "new",
    hovered_id = state.hover, pressed_id = state.pressed,
    rect = { x = cx, y = layout.height - 76, w = 132, h = 32, cx = cx + 66, cy = layout.height - 60 } })
  Button.render(ui, { key = "detach", label = "detach", action = "detach",
    hovered_id = state.hover, pressed_id = state.pressed,
    rect = { x = cx + 144, y = layout.height - 76, w = 100, h = 32, cx = cx + 194, cy = layout.height - 60 } })
end

return M
