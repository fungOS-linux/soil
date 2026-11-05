return {
  root = "notes",
  selected = "notes/welcome.md",
  notes = {
    { path = "notes/welcome.md", title = "Welcome", selected = true },
  },
  document = {
    path = "notes/welcome.md",
    title = "Welcome",
    lines = {},
  },
  editor = {
    text = "# Welcome\n\nA local-first notes workspace.\n",
    expected_version = 3,
    dirty = true,
  },
}
