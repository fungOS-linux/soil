//! Local CRM seed and read-only inspector.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use soil_crm_host::{
    assign_company, default_root, link_contact, seed_mock, snapshot, unassign_company,
    unlink_contact,
};

#[derive(Parser)]
#[command(version, about = "CRM over fungos-records")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write deterministic mock CRM data.
    Seed {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "personal/avery/crm")]
        book: String,
    },
    /// Link a private contact to an account without sharing it.
    Link {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "personal/avery/crm")]
        book: String,
        #[arg(long)]
        account: String,
        #[arg(long)]
        contact: String,
        #[arg(long, default_value = "")]
        role: String,
    },
    /// Remove a private account-contact link and any projection for it.
    Unlink {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "personal/avery/crm")]
        book: String,
        #[arg(long)]
        account: String,
        #[arg(long)]
        contact: String,
    },
    /// Link an account to one of our companies/brands.
    Assign {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "personal/avery/crm")]
        book: String,
        #[arg(long)]
        account: String,
        #[arg(long)]
        company: String,
        #[arg(long, value_delimiter = ',', default_value = "customer")]
        role: Vec<String>,
    },
    /// Remove an account-to-company link.
    Unassign {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "personal/avery/crm")]
        book: String,
        #[arg(long)]
        account: String,
        #[arg(long)]
        company: String,
    },
    /// Serve a read-only local CRM inspector.
    Serve {
        #[arg(long)]
        root: Option<PathBuf>,
        #[arg(long, default_value = "personal/avery/crm")]
        book: String,
        #[arg(long, default_value = "127.0.0.1:8790")]
        listen: String,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::Seed { root, book } => {
            let root = root.unwrap_or_else(default_root);
            let count = seed_mock(&root, &book)?;
            println!(
                "seeded {count} mock CRM records into {} / {book}",
                root.display()
            );
        }
        Cmd::Link {
            root,
            book,
            account,
            contact,
            role,
        } => {
            let root = root.unwrap_or_else(default_root);
            let id = link_contact(&root, &book, &account, &contact, &role)?;
            println!("linked privately as {id}; no team projection created");
        }
        Cmd::Unlink {
            root,
            book,
            account,
            contact,
        } => {
            let root = root.unwrap_or_else(default_root);
            unlink_contact(&root, &book, &account, &contact)?;
            println!("unlinked {contact} from {account}; projection removed if present");
        }
        Cmd::Assign {
            root,
            book,
            account,
            company,
            role,
        } => {
            let root = root.unwrap_or_else(default_root);
            let id = assign_company(&root, &book, &account, &company, &role)?;
            println!("assigned {account} to {company} as {id}");
        }
        Cmd::Unassign {
            root,
            book,
            account,
            company,
        } => {
            let root = root.unwrap_or_else(default_root);
            unassign_company(&root, &book, &account, &company)?;
            println!("unassigned {account} from {company}");
        }
        Cmd::Serve { root, book, listen } => {
            let root = root.unwrap_or_else(default_root);
            serve(&root, &book, &listen)?;
        }
    }
    Ok(())
}

fn serve(root: &std::path::Path, book: &str, listen: &str) -> anyhow::Result<()> {
    let server = tiny_http::Server::http(listen).map_err(|error| anyhow::anyhow!("{error}"))?;
    println!(
        "soil crm: http://{listen}/  book={book} root={}",
        root.display()
    );
    for request in server.incoming_requests() {
        let url = request.url().to_string();
        if url.starts_with("/api/crm") {
            let view = if url.contains("view=private") {
                "private"
            } else {
                "team"
            };
            let body = match snapshot(root, book, view) {
                Ok(body) => body,
                Err(error) => serde_json::json!({ "error": error.to_string() }),
            };
            let text = serde_json::to_string(&body)?;
            respond(request, "application/json", text.as_bytes())?;
        } else if url == "/" {
            respond(request, "text/html; charset=utf-8", HTML.as_bytes())?;
        } else {
            respond(request, "text/plain", b"not found\n")?;
        }
    }
    Ok(())
}

fn respond(request: tiny_http::Request, content_type: &str, body: &[u8]) -> anyhow::Result<()> {
    let header = tiny_http::Header::from_bytes("Content-Type", content_type)
        .map_err(|_| anyhow::anyhow!("invalid header"))?;
    request.respond(tiny_http::Response::from_data(body.to_vec()).with_header(header))?;
    Ok(())
}

const HTML: &str = r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Soil CRM</title>
<style>
:root { color-scheme: dark; --bg:#111; --panel:#191919; --line:#303030; --text:#ececec; --muted:#999; --accent:#b8a1ff; }
* { box-sizing:border-box }
body { margin:0; background:var(--bg); color:var(--text); font:14px/1.4 ui-sans-serif,system-ui,sans-serif }
header { display:flex; align-items:center; gap:12px; padding:12px 18px; border-bottom:1px solid var(--line); flex-wrap:wrap }
header h1 { font-size:16px; margin:0; font-weight:600 }
header .spacer { flex:1 }
#companies { display:flex; gap:6px; flex-wrap:wrap }
button { background:#242424; color:var(--text); border:1px solid var(--line); border-radius:6px; padding:6px 10px; cursor:pointer }
button.active { border-color:var(--accent); color:var(--accent) }
main { display:grid; grid-template-columns:320px 1fr; min-height:calc(100vh - 58px) }
#accounts { border-right:1px solid var(--line); overflow:auto }
.account { padding:14px 18px; border-bottom:1px solid var(--line); cursor:pointer }
.account:hover,.account.selected { background:var(--panel) }
.account b { display:block; font-size:14px }
.account span { color:var(--muted); font-size:12px }
#detail { padding:22px 26px; overflow:auto }
h2 { margin:0 0 4px; font-size:22px }
h3 { margin:26px 0 10px; font-size:12px; text-transform:uppercase; letter-spacing:.08em; color:var(--muted) }
.grid { display:grid; grid-template-columns:repeat(auto-fit,minmax(180px,1fr)); gap:10px }
.card { border:1px solid var(--line); background:var(--panel); border-radius:8px; padding:12px }
.card b { display:block; margin-bottom:4px }
.muted { color:var(--muted) }
.row { padding:10px 0; border-bottom:1px solid var(--line) }
.row:last-child { border-bottom:0 }
.pill { display:inline-block; font-size:11px; border:1px solid var(--line); border-radius:999px; padding:2px 7px; color:var(--muted) }
</style>
<body>
<header>
  <h1>Soil CRM</h1>
  <span class="pill" id="book"></span>
  <div id="companies"></div>
  <span class="spacer"></span>
  <button id="team" class="active" onclick="setView('team')">Team view</button>
  <button id="private" onclick="setView('private')">Private view</button>
</header>
<main>
  <aside id="accounts"></aside>
  <section id="detail"><p class="muted">Loading…</p></section>
</main>
<script>
let data = null, companyId = null, selected = null;
const esc = (value) => String(value ?? '').replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
function currentCompany() {
  return (data?.companies || []).find(company => company.id === companyId);
}
async function load(view) {
  const response = await fetch('/api/crm?view=' + view);
  data = await response.json();
  document.getElementById('team').classList.toggle('active', view === 'team');
  document.getElementById('private').classList.toggle('active', view === 'private');
  document.getElementById('book').textContent = view + ' projection';
  const companies = data.companies || [];
  if (!companyId || !companies.some(company => company.id === companyId)) {
    companyId = companies[0]?.id || null;
  }
  const company = currentCompany();
  if (!selected || !(company?.accounts || []).some(account => account.id === selected)) {
    selected = company?.accounts?.[0]?.id || null;
  }
  renderCompanies();
  renderAccounts();
  render();
}
function renderCompanies() {
  const list = document.getElementById('companies');
  list.replaceChildren();
  for (const company of data?.companies || []) {
    const button = document.createElement('button');
    button.textContent = company.name + (company.accounts?.length ? ' ' + company.accounts.length : '');
    button.className = company.id === companyId ? 'active' : '';
    button.onclick = () => {
      companyId = company.id;
      selected = company.accounts?.[0]?.id || null;
      renderCompanies();
      renderAccounts();
      render();
    };
    list.append(button);
  }
}
function renderAccounts() {
  const list = document.getElementById('accounts');
  list.replaceChildren();
  const company = currentCompany();
  for (const account of company?.accounts || []) {
    const node = document.createElement('div');
    node.className = 'account' + (account.id === selected ? ' selected' : '');
    node.onclick = () => { selected = account.id; renderAccounts(); render(); };
    const b = document.createElement('b'); b.textContent = account.name;
    const roles = account.relationship?.roles || [];
    const s = document.createElement('span'); s.textContent = roles.join(' · ') || 'account';
    node.append(b, s); list.append(node);
  }
  if (!(company?.accounts || []).length) {
    const empty = document.createElement('p');
    empty.className = 'muted';
    empty.style.padding = '18px';
    empty.textContent = 'No accounts in this brand yet.';
    list.append(empty);
  }
}
function render() {
  const company = currentCompany();
  const account = (company?.accounts || []).find(entry => entry.id === selected);
  const detail = document.getElementById('detail');
  if (!account) {
    detail.innerHTML = '<p class="muted">No account selected.</p>';
    return;
  }
  const roles = (account.relationship?.roles || []).map(role => `<span class="pill">${esc(role)}</span>`).join(' ');
  detail.innerHTML = `<h2>${esc(account.name)}</h2>
    <div class="muted">${esc((account.domains || []).join(', '))} · ${esc(account.industry || '')} · ${roles}</div>
    <h3>Contacts</h3><div class="grid">${(account.contacts || []).map(c => `<div class="card"><b>${esc(c.name || c.first_name)}</b><span class="muted">${esc(c.email || c.contact_token || '')}</span></div>`).join('') || '<p class="muted">None</p>'}</div>
    <h3>Projects</h3>${(account.projects || []).map(p => `<div class="row"><b>${esc(p.name)}</b> <span class="pill">${esc(p.status)}</span> <span class="pill">${esc(p.company_role || 'owner')}</span><div class="muted">${esc(p.kind || '')} · due ${esc(p.due_at || '—')}</div><div>${esc(p.summary)}</div></div>`).join('') || '<p class="muted">None</p>'}
    <h3>Opportunities</h3>${(account.opportunities || []).map(o => `<div class="row"><b>${esc(o.name)}</b> <span class="pill">${esc(o.stage)}</span><div class="muted">$${Number(o.amount || 0).toLocaleString()} · close ${esc(o.close_date || '—')}</div></div>`).join('') || '<p class="muted">None</p>'}
    <h3>Activity</h3>${(account.activities || []).map(a => `<div class="row"><b>${esc(a.name)}</b> <span class="pill">${esc(a.kind)}</span><div class="muted">${esc(a.occurred_at)} · ${esc(a.contact_token || '')}${a.project_id ? ' · ' + esc(a.project_id) : ''}</div><div>${esc(a.description)}</div></div>`).join('') || '<p class="muted">None</p>'}`;
}
function setView(view) { load(view); }
load('team');
</script>
</body>
</html>
"#;
