-- Wrapped text. The host measures with the same wrapping it will draw with, so
-- a block knows its height before it paints and nothing has to be guessed.
local M = {}

function M.render(ui, props)
  local t = ui.theme
  local size = props.size or t.font.size.body
  local width, height = ui.measure(props.text, { size = size, wrap = props.width })
  ui.text(props.text, { x = props.x, y = props.y }, {
    size = size,
    colour = props.colour or t.colour.muted,
    anchor = "top",
    wrap = props.width,
  })
  return width, height
end

return M
