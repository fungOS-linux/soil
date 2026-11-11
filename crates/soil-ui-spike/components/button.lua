-- A component is a file. It takes props, it paints, it registers its own hits.
-- Anything it registers is scoped to its instance key, so two buttons can share
-- a label without sharing an id.
local M = {}

function M.render(ui, props)
  local t = ui.theme
  ui.scope(props.key or props.label, function()
    local id = ui.id(props.label)
    local pressed = props.pressed_id == id
    local hover = props.hovered_id == id
    local fill = pressed and t.colour.primary_pressed
      or (hover and t.colour.primary_raised or t.colour.primary)
    ui.rect(props.rect, fill, t.radius.md)
    -- A press moves the label down a pixel: the cheapest "this is happening"
    -- signal there is, and it needs no extra colour.
    local lift = pressed and 1 or 0
    ui.text(props.label, { x = props.rect.cx, y = props.rect.cy + lift },
      { colour = t.colour.on_primary, size = t.font.size.small, align = "center" })
    ui.hit(props.label, props.rect, props.action or props.label)
  end)
end

return M
