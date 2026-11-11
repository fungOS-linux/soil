-- Loader and recorder. Everything lives on `SOIL` because the host calls
-- into this file once per frame: the setup chunk defines it, then each frame
-- is a tiny chunk that calls `SOIL.render`. Locals would not survive that.
--
-- `dofile`/`require` hand back files the bundle already carries: a spore ships
-- its sources, so nothing reaches for a filesystem.
package = { path = "" }

SOIL = { modules = {}, loaded = {}, commands = {}, host = {} }

function SOIL.resolve(name)
  local path = name
  if not path:match("%.lua$") then path = name:gsub("%.", "/") .. ".lua" end
  if SOIL.loaded[path] then return SOIL.loaded[path] end
  local chunk = SOIL.modules[path]
  if not chunk then error(name .. " is not bundled", 2) end
  SOIL.loaded[path] = chunk()
  return SOIL.loaded[path]
end

dofile = SOIL.resolve
require = SOIL.resolve

local function push(command)
  local commands = SOIL.commands
  commands[#commands + 1] = command
end

function SOIL.host.rect(x, y, w, h, r, colour)
  push({ "rect", x, y, w, h, r, colour })
end

function SOIL.host.text(value, x, y, size, colour, anchor, align, font, wrap)
  push({ "text", value, x, y, size, colour, anchor, align, font, wrap })
end

function SOIL.host.hit(id, x, y, w, h, action)
  push({ "hit", id, x, y, w, h, action })
end

function SOIL.host.group(key)
  push({ "group", key })
end

function SOIL.host.clip(x, y, w, h)
  if w then push({ "clip", x, y, w, h }) else push({ "clip-end" }) end
end

-- Stand-in for the shaper. The native host measures with parley; the page only
-- needs a plausible number to place things, and it re-wraps when it paints.
function SOIL.host.measure(value, size, _font, _wrap)
  return #value * size * 0.52, size * 1.25
end
