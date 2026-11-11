-- A view with N rows, for the frame budget. Reads its own knobs from the
-- environment because it is a benchmark, not a spore.
local lib = dofile("ui.lua")
local theme = dofile("theme.lua")

local ROWS = tonumber(os.getenv("SOIL_ROWS") or "1000")
local VIRTUAL = os.getenv("SOIL_VIRTUAL") ~= nil

local M = {}

function M.render(host, layout, state)
  local ui = lib.new(host, theme)
  local t = theme
  local row_h = 28

  ui.rect({ x = 0, y = 0, w = layout.width, h = layout.height }, t.colour.background)

  local function row(i, rect)
    local fill = (i % 2 == 0) and t.colour.raised or t.colour.surface
    if state.hover == ("row." .. i) then fill = t.colour.primary end
    ui.rect(rect, fill, t.radius.sm)
    ui.text("row " .. i .. " — a label that has to be shaped",
      { x = rect.x + 12, y = rect.y + rect.h / 2 },
      { size = t.font.size.small, colour = t.colour.text })
    ui.hit("row." .. i, rect, "open:" .. i)
  end

  if VIRTUAL then
    ui.list({ x = 8, y = 8, w = layout.width - 16, h = layout.height - 16 },
      ROWS, row_h, state.scroll or 0, row)
  else
    local y = 8
    for i = 1, ROWS do
      row(i, { x = 8, y = y, w = layout.width - 16, h = row_h - 4,
               cx = 8 + (layout.width - 16) / 2, cy = y + (row_h - 4) / 2 })
      y = y + row_h
    end
  end
end

return M
