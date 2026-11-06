return {
  selected = "c-ada",
  contacts = {
    { id = "c-ada", name = "Ada Lovelace", email = "ada@acme.example", selected = true },
    { id = "c-grace", name = "Grace Hopper", email = "grace@acme.example" },
  },
  contact = {
    id = "c-ada",
    name = "Ada Lovelace",
    email = "ada@acme.example",
    phone = "+1 555 0101",
    title = "CTO",
    contact_token = "espo:c1",
    shared = true,
  },
  shared = {
    { contact_token = "espo:c1", first_name = "Ada", account_id = "espo:account:a1" },
  },
}
