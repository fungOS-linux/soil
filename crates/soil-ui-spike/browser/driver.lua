-- Frame driver. The host calls SOIL.render once per frame with what it knows:
-- the extent, the pointer, hover/pressed/focus ids and the selection. The view
-- styles from that; nothing here tracks input on its own.

local function quote(value)
  local text = tostring(value)
  text = text:gsub("\\", "\\\\"):gsub('"', '\\"')
  text = text:gsub("\n", "\\n"):gsub("\r", "\\r"):gsub("\t", "\\t")
  return '"' .. text .. '"'
end

function SOIL.encode(value)
  local kind = type(value)
  if kind == "number" then
    if value ~= value or value == math.huge or value == -math.huge then return "0" end
    return string.format("%.6g", value)
  elseif kind == "string" then
    return quote(value)
  elseif kind == "boolean" then
    return tostring(value)
  elseif kind == "table" then
    local parts = {}
    for i = 1, #value do parts[i] = SOIL.encode(value[i]) end
    return "[" .. table.concat(parts, ",") .. "]"
  end
  return "null"
end

function SOIL.render(frame)
  local width = math.max(1, math.floor(frame.width or 640))
  local height = math.max(1, math.floor(frame.height or 360))
  -- Spore-side layout: one fixed sidebar until there is a breakpoint rule.
  local sidebar = 200

  local layout = {
    width = width,
    height = height,
    sidebar = { width = sidebar },
    content = { x = sidebar, y = 0 },
  }
  local state = {
    targets = { "studio", "neo", "acme" },
    selected = frame.selected or "studio",
    hover = frame.hover or "",
    pressed = frame.pressed or "",
    focus = frame.focus or "",
    pointer = frame.pointer or { x = 0, y = 0 },
    mods = { cmd = false, shift = false, alt = false, ctrl = false },
  }

  SOIL.commands = {}
  SOIL.resolve("view.lua").render(SOIL.host, layout, state)
  return SOIL.encode(SOIL.commands)
end
