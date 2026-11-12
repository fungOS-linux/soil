-- A row: pill when selected or hovered, accent bar, label, hit region.
local M = {}

function M.render(ui, props)
  local t = ui.theme
  ui.scope(props.key or props.label, function()
    local hovered = props.hovered_id == ui.id(props.label)
    local focused = props.focused_id == ui.id(props.label)
    if props.selected then
      ui.rect(props.rect, t.colour.raised, t.radius.md)
      ui.rect({ x = props.rect.x + 6, y = props.rect.cy - 7, w = 3, h = 14 }, t.colour.primary, 2)
    elseif hovered then
      ui.rect(props.rect, t.colour.raised, t.radius.md)
    end
    -- Focus is a ring on the right edge plus accent text: no stroke primitive
    -- yet, and a shifted edge reads more clearly than a colour change alone.
    if focused then
      ui.rect({ x = props.rect.x + props.rect.w - 5, y = props.rect.cy - 7, w = 2, h = 14 }, t.colour.primary, 1)
    end
    local content = ui.box(props.rect, t.space.md)
    ui.text(props.label, { x = content.x + 6, y = props.rect.cy },
      { colour = (props.selected or hovered) and t.colour.text or (focused and t.colour.primary or t.colour.muted) })
    ui.hit(props.label, props.rect, props.action or ("open:" .. props.label))
  end)
end

return M
