return {
  selected = "a-acme",
  accounts = {
    { id = "a-acme", name = "Acme Robotics", selected = true, domains = { "acme.example" } },
    { id = "a-northstar", name = "Northstar Studio" },
  },
  account = {
    id = "a-acme",
    name = "Acme Robotics",
    domains = { "acme.example" },
    contacts = {
      { name = "Ada Lovelace", role = "CTO", contact_token = "t-ada" },
    },
    timeline = {
      { kind = "call", title = "Discovery call" },
      { kind = "message", title = "Following up on the pilot" },
    },
  },
}
