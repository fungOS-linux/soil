return {
  root = "notes",
  selected = "notes/welcome.md",
  notes = {
    { path = "notes/welcome.md", title = "Welcome", selected = true },
    { path = "notes/projects/soil.md", title = "Soil", updated_at_ms = 0 },
    { path = "notes/reference/dcp.md", title = "DCP" },
  },
  document = {
    path = "notes/welcome.md",
    title = "Welcome",
    lines = {
      { text = "Welcome", level = 1 },
      { text = "A local-first notes workspace." },
      { text = "Links", level = 2 },
      { text = "See [[projects/soil]] and [[reference/dcp]]." },
    },
  },
  backlinks = {
    { source = "notes/projects/soil.md", target = "notes/welcome.md" },
  },
}
