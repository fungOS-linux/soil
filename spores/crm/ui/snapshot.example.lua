return {
  mode = "pipeline",
  selected = "a-acme",
  pipeline = {
    stages = {
      {
        stage = "proposal",
        count = 1,
        amount = 50000,
        opportunities = {
          { id = "opp-1", name = "Acme pilot", account_name = "Acme Robotics", amount = 50000 },
        },
      },
      {
        stage = "won",
        count = 1,
        amount = 120000,
        opportunities = {
          { id = "opp-2", name = "Northstar build", account_name = "Northstar Studio", amount = 120000 },
        },
      },
    },
  },
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
