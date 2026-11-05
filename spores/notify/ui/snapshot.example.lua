return {
  focus = { mode = "work", until_ms = 0, held_count = 3 },
  notifications = {
    { id = "n1", app_id = "dev.fungos.soil", title = "Build failed", body = "soil-runtime failed", priority = "active" },
    { id = "n2", app_id = "dev.fungos.mail", title = "Ada replied", body = "Re: notes", priority = "passive" },
  },
  held = {
    { id = "h1", app_id = "dev.fungos.github", title = "3 pull requests", body = "held until focus ends", priority = "passive", held = true },
  },
  history = {},
}
