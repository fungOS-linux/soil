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
    local fill = pressed and t.colour.primary or (hover and t.colour.primary or t.colour.primary)
    ui.rect(props.rect, fill, t.radius.md)
    ui.text(props.label, { x = props.rect.cx, y = props.rect.cy },
      { colour = t.colour.on_primary, size = t.font.size.small, align = "center" })
    ui.hit(props.label, props.rect, props.action or props.label)
  end)
end

return M
