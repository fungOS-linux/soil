-- DFM report UI. The host supplies the report snapshot; this view is a pure
-- projection and emits action intents only. The 3D view is a separate native
-- surface (`soil-dfm-viz`), opened by intent, never drawn here.
local ui = require("fungos.ui")
local action = require("fungos.action")

local M = { name = "dev.fungos.dfm", version = "0.1.0" }

local SEVERITY_INK = { blocker = "primary", warning = "text", note = "muted" }

local function clip(value, limit)
  value = tostring(value or "")
  if #value <= limit then return value end
  return value:sub(1, limit - 1) .. "…"
end

local function fmt(value)
  if value == nil then return "—" end
  if math.type(value) == "integer" then return tostring(value) end
  return string.format("%.3f", value)
end

local function selected_finding(snapshot)
  for index, finding in ipairs(snapshot.findings or {}) do
    if snapshot.selected == index or finding.selected then return finding, index end
  end
  return (snapshot.findings or {})[1], 1
end

local function counts(snapshot)
  local summary = snapshot.summary or {}
  return ui.row("dfm-counts", { height = 22, gap = 12 }, {
    ui.text("dfm-count-warnings", tostring(summary.warnings or 0) .. " warnings", { size = 10, ink = "text" }),
    ui.text("dfm-count-notes", tostring(summary.notes or 0) .. " notes", { size = 10, ink = "muted" }),
    ui.text("dfm-count-blockers", tostring(summary.blockers or 0) .. " blocking", {
      size = 10,
      ink = (summary.blockers or 0) > 0 and "primary" or "muted",
    }),
  })
end

local function header(ctx, snapshot)
  return ui.column("dfm-header", { height = 62, gap = 2, padding = { left = 16, right = 16, top = 10 } }, {
    ui.text("dfm-title", snapshot.part or "part", { size = 15 }),
    ui.text("dfm-subtitle", (snapshot.process or "process") .. " · " ..
      tostring(#(snapshot.findings or {})) .. " finding(s)", { size = 10, ink = "muted" }),
    counts(snapshot),
  })
end

local function finding_row(ctx, finding, index, selected)
  local pinned = finding.anchor ~= nil
  return ui.list_row("dfm-finding-" .. index, {
    height = 54,
    selected = selected,
    action = action.intent("dfm.select", { index = index }),
  }, {
    ui.column("dfm-finding-" .. index .. "-body", { flex_grow = 1, gap = 1 }, {
      ui.text("dfm-finding-" .. index .. "-rule", clip(finding.rule, 34), {
        size = 11,
        ink = SEVERITY_INK[finding.severity] or "text",
      }),
      ui.text(
        "dfm-finding-" .. index .. "-measure",
        fmt(finding.measured) .. " vs " .. fmt(finding.limit),
        { size = 10, ink = "muted" }
      ),
    }),
    ui.text("dfm-finding-" .. index .. "-pin", pinned and "3D" or "—", {
      size = 9,
      ink = pinned and "primary" or "muted",
      width = 26,
    }),
  })
end

local function rail(ctx, snapshot)
  local selected = select(2, selected_finding(snapshot))
  local children = { ui.section_label("dfm-rail-label", "FINDINGS", { ink = "muted" }) }
  for index, finding in ipairs(snapshot.findings or {}) do
    ui.push(children, finding_row(ctx, finding, index, index == selected))
  end
  if #(snapshot.findings or {}) == 0 then
    ui.push(children, ui.empty_state("dfm-empty", "No findings", "This report came back clean.", {}))
  end
  return ui.sidebar("dfm-rail", { width = 300 }, children)
end

local function detail(ctx, snapshot)
  local finding = selected_finding(snapshot)
  if not finding then
    return ui.pane("dfm-detail", {}, {
      ui.empty_state("dfm-detail-empty", "Nothing selected", "Pick a finding on the left.", {}),
    })
  end

  local body = {
    ui.row("dfm-detail-top", { height = 30, gap = 8, align_items = "center" }, {
      ui.badge("dfm-detail-severity", (finding.severity or "note"):upper(), {
        ink = SEVERITY_INK[finding.severity] or "muted",
      }),
      ui.text("dfm-detail-rule", finding.rule or "finding", { size = 14, flex_grow = 1 }),
    }),
    ui.rule("dfm-detail-rule-line", { margin = { top = 6, bottom = 6 } }),
    ui.text("dfm-detail-measure", "measured " .. fmt(finding.measured), { size = 11, height = 18 }),
    ui.text("dfm-detail-limit", "limit    " .. fmt(finding.limit), { size = 11, height = 18 }),
  }
  if finding.basis then
    ui.push(body, ui.text("dfm-detail-basis", "basis    " .. tostring(finding.basis), {
      size = 11,
      height = 18,
      ink = "muted",
    }))
  end
  ui.push(body, ui.spacer("dfm-detail-gap", { height = 8 }))
  ui.push(body, ui.text("dfm-detail-message", finding.message or "", {
    size = 12,
    wrap = true,
    height = 96,
    ink = "text",
  }))
  if finding.geometry then
    ui.push(body, ui.text("dfm-detail-geometry", "geometry  " .. tostring(finding.geometry), {
      size = 10,
      height = 18,
      ink = "muted",
    }))
  end
  if finding.note and #finding.note > 0 then
    ui.push(body, ui.text("dfm-detail-note", "note  " .. finding.note, {
      size = 11,
      height = 20,
      ink = "muted",
    }))
  end
  ui.push(body, ui.spacer("dfm-detail-gap2", { flex_grow = 1 }))
  ui.push(body, ui.action_bar("dfm-detail-actions", {
    {
      id = "open-native",
      label = finding.anchor and "Open 3D view" or "No geometry to show",
      variant = "primary",
      disabled = finding.anchor == nil,
      action = action.intent("dfm.open_native", { index = snapshot.selected }),
    },
    { spacer = true },
    {
      id = "note",
      label = "Leave note",
      action = action.intent("dfm.note", { index = snapshot.selected }),
    },
  }, { height = 42 }))

  return ui.pane("dfm-detail", { padding = { left = 18, right = 18, top = 14, bottom = 12 } }, body)
end

function M.render(ctx)
  local snapshot = ctx and ctx.snapshot or {}
  return ui.workspace("dfm", { flex_grow = 1 }, {
    header(ctx, snapshot),
    ui.row("dfm-body", { flex_grow = 1 }, {
      rail(ctx, snapshot),
      detail(ctx, snapshot),
    }),
  })
end

return M
