//! Local CRM seed and read-only inspector.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use soil_crm_host::{default_root, link_contact, seed_mock, snapshot, unlink_contact};

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
header { display:flex; align-items:center; gap:16px; padding:14px 18px; border-bottom:1px solid var(--line); }
header h1 { font-size:16px; margin:0; font-weight:600 }
header .spacer { flex:1 }
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
  <span class="spacer"></span>
  <button id="team" class="active" onclick="setView('team')">Team view</button>
  <button id="private" onclick="setView('private')">Private view</button>
</header>
<main>
  <aside id="accounts"></aside>
  <section id="detail"><p class="muted">Loading…</p></section>
</main>
<script>
let data = null, selected = null;
async function load(view) {
  const response = await fetch('/api/crm?view=' + view);
  data = await response.json();
  document.getElementById('team').classList.toggle('active', view === 'team');
  document.getElementById('private').classList.toggle('active', view === 'private');
  document.getElementById('book').textContent = view + ' projection';
  const list = document.getElementById('accounts');
  list.replaceChildren();
  for (const account of data.accounts || []) {
    const node = document.createElement('div');
    node.className = 'account' + (account.id === selected ? ' selected' : '');
    node.onclick = () => { selected = account.id; render(); load(document.getElementById('team').classList.contains('active') ? 'team' : 'private'); };
    const b = document.createElement('b'); b.textContent = account.name;
    const s = document.createElement('span'); s.textContent = (account.opportunities || []).map(o => o.stage).join(' · ');
    node.append(b, s); list.append(node);
  }
  if (!selected && data.accounts?.length) selected = data.accounts[0].id;
  render();
}
function render() {
  const account = (data?.accounts || []).find(a => a.id === selected);
  const detail = document.getElementById('detail');
  if (!account) { detail.innerHTML = '<p class="muted">No accounts yet.</p>'; return; }
  const esc = (value) => String(value ?? '').replace(/[&<>"]/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
  detail.innerHTML = `<h2>${esc(account.name)}</h2>
    <div class="muted">${esc((account.domains || []).join(', '))} · ${esc(account.industry || '')}</div>
    <h3>Contacts</h3><div class="grid">${(account.contacts || []).map(c => `<div class="card"><b>${esc(c.name || c.first_name)}</b><span class="muted">${esc(c.email || c.contact_token || '')}</span></div>`).join('') || '<p class="muted">None</p>'}</div>
    <h3>Opportunities</h3>${(account.opportunities || []).map(o => `<div class="row"><b>${esc(o.name)}</b> <span class="pill">${esc(o.stage)}</span><div class="muted">$${Number(o.amount || 0).toLocaleString()} · close ${esc(o.close_date || '—')}</div></div>`).join('') || '<p class="muted">None</p>'}
    <h3>Activity</h3>${(account.activities || []).map(a => `<div class="row"><b>${esc(a.name)}</b> <span class="pill">${esc(a.kind)}</span><div class="muted">${esc(a.occurred_at)} · ${esc(a.contact_token || '')}</div><div>${esc(a.description)}</div></div>`).join('') || '<p class="muted">None</p>'}`;
}
function setView(view) { load(view); }
load('team');
</script>
</body>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linking_is_private_until_explicitly_projected() {
        let root = tempfile::tempdir().unwrap();
        seed_mock(root.path(), "personal/test/crm").unwrap();
        unlink_contact(root.path(), "personal/test/crm", "a-acme", "c-grace").unwrap();
        link_contact(
            root.path(),
            "personal/test/crm",
            "a-harbor",
            "c-grace",
            "Advisor",
        )
        .unwrap();

        let team = snapshot(root.path(), "personal/test/crm", "team").unwrap();
        let private = snapshot(root.path(), "personal/test/crm", "private").unwrap();
        let harbor_team = team["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["id"] == "a-harbor")
            .unwrap();
        assert!(harbor_team["contacts"].as_array().unwrap().is_empty());
        let harbor_private = private["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|account| account["id"] == "a-harbor")
            .unwrap();
        let grace = harbor_private["contacts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|contact| contact["id"] == "c-grace")
            .unwrap();
        assert_eq!(grace["shared"], false);
        assert_eq!(grace["email"], "grace@acme.example");
    }

    #[test]
    fn mock_snapshot_has_private_and_team_views() {
        let root = tempfile::tempdir().unwrap();
        seed_mock(root.path(), "personal/test/crm").unwrap();
        let team = snapshot(root.path(), "personal/test/crm", "team").unwrap();
        let private = snapshot(root.path(), "personal/test/crm", "private").unwrap();
        assert_eq!(team["accounts"].as_array().unwrap().len(), 3);
        let team_contact = &team["accounts"][0]["contacts"][0];
        assert!(team_contact.get("email").is_none(), "{team_contact}");
        let private_contact = &private["accounts"][0]["contacts"][0];
        assert!(private_contact.get("email").is_some(), "{private_contact}");
    }
}
