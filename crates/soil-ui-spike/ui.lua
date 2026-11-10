-- The styling layer. Pure Lua on top of six host primitives:
--   rect, text, hit, measure, clip, load_font
-- Everything here is a file edit, never a rebuild.
local M = {}

function M.new(host, theme)
  local ui = { theme = theme, host = host }

  function ui.rect(rect, colour, radius)
    host.rect(rect.x, rect.y, rect.w, rect.h, radius or 0, colour)
  end

  function ui.text(value, at, opts)
    opts = opts or {}
    return host.text(value, at.x, at.y, opts.size or theme.font.size.body,
      opts.colour or theme.colour.text, opts.anchor or "middle",
      opts.align or "left", opts.font or "body")
  end

  function ui.measure(value, opts)
    opts = opts or {}
    return host.measure(value, opts.size or theme.font.size.body, opts.font or "body")
  end

  -- Hit ids are scoped to the component instance that registered them, so two
  -- buttons may share a label without sharing an id.
  local prefix = ""

  function ui.scope(id, paint)
    local previous = prefix
    prefix = previous .. tostring(id) .. "."
    paint()
    prefix = previous
  end

  -- The fully-scoped id for a name, for comparing against host state.
  function ui.id(name)
    return prefix .. tostring(name)
  end

  function ui.hit(id, rect, action)
    host.hit(prefix .. tostring(id), rect.x, rect.y, rect.w, rect.h, action or "")
  end

  function ui.clip(rect, paint)
    host.clip(rect.x, rect.y, rect.w, rect.h)
    paint()
    host.clip()
  end

  -- A box with padding returns its content rect. No call site does this arithmetic.
  function ui.box(rect, pad)
    pad = pad or theme.space.md
    return { x = rect.x + pad, y = rect.y + pad,
             w = rect.w - pad * 2, h = rect.h - pad * 2,
             cx = rect.x + rect.w / 2, cy = rect.y + rect.h / 2 }
  end

  function ui.row(rect, label, state)
    local t = theme
    if state.selected then
      ui.rect(rect, t.colour.raised, t.radius.md)
      ui.rect({ x = rect.x + 6, y = rect.y + (rect.h - 14) / 2, w = 3, h = 14 }, t.colour.primary, 2)
    elseif state.hover then
      ui.rect(rect, t.colour.raised, t.radius.md)
    end
    local content = ui.box(rect, t.space.md)
    ui.text(label, { x = content.x + 6, y = rect.y + rect.h / 2 },
      { colour = state.selected and t.colour.text or (state.hover and t.colour.text or t.colour.muted) })
    ui.hit(label, rect, "open:" .. label)
  end

  function ui.button(rect, label, state)
    local t = theme
    local fill = state.pressed and t.colour.primary or t.colour.primary
    ui.rect(rect, fill, t.radius.md)
    ui.text(label, { x = rect.x + rect.w / 2, y = rect.y + rect.h / 2 },
      { colour = t.colour.on_primary, size = t.font.size.small, align = "center" })
    ui.hit(label, rect, label)
  end

  return ui
end

return M
