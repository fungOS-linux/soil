use std::sync::Mutex;

use anyhow::Result;
use fungos_records::Store;
use serde_json::json;
use soil_records_host::{Namespace, NamespacedRecordsHost, add_to_linker, records as api};
use wasmtime::{
    Config, Engine, Store as WasmStore,
    component::{Component, Linker, ResourceTable},
};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({ path: "../../spores/crm/wit", world: "crm" });

struct Host {
    records: Mutex<NamespacedRecordsHost>,
    table: ResourceTable,
    wasi: WasiCtx,
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl api::Host for Host {
    fn get(&mut self, key: api::Key) -> std::result::Result<api::Entry, api::Error> {
        self.records.get_mut().unwrap().get(key)
    }
    fn scan(
        &mut self,
        collection: String,
        cursor: Option<String>,
        limit: u32,
    ) -> std::result::Result<api::Page, api::Error> {
        self.records
            .get_mut()
            .unwrap()
            .scan(collection, cursor, limit)
    }
    fn upsert(&mut self, entry: api::Entry) -> std::result::Result<api::Entry, api::Error> {
        self.records.get_mut().unwrap().upsert(entry)
    }
    fn delete(&mut self, key: api::Key) -> std::result::Result<(), api::Error> {
        self.records.get_mut().unwrap().delete(key)
    }
    fn changes(
        &mut self,
        since: Option<String>,
        limit: u32,
    ) -> std::result::Result<api::Page, api::Error> {
        self.records.get_mut().unwrap().changes(since, limit)
    }
    fn apply(
        &mut self,
        entries: Vec<api::Entry>,
    ) -> std::result::Result<api::ApplyResult, api::Error> {
        self.records.get_mut().unwrap().apply(entries)
    }
}

fn seed(root: &std::path::Path) -> Result<()> {
    let mut store = Store::open(root, &root.join("device_id"))?;
    let crm = "team/test/crm";
    let contacts = "personal/test/contacts";
    let messages = "personal/test/messages";
    let calls = "personal/test/calls";
    let put = |store: &mut Store, book: &str, id: &str, schema: &str, body: serde_json::Value| {
        store.put_record(book, id, schema, 1, body)
    };
    put(
        &mut store,
        crm,
        "co-fpl",
        "fungos.crm/company@1",
        json!({"name": "FPL", "status": "active"}),
    )?;
    put(
        &mut store,
        crm,
        "a-acme",
        "fungos.crm/account@1",
        json!({
            "name": "Acme Robotics", "domains": ["acme.example"], "industry": "Manufacturing", "owner": "Avery"
        }),
    )?;
    put(
        &mut store,
        crm,
        "acm-a-acme-co-fpl",
        "fungos.crm/account-company@1",
        json!({
            "account_id": "a-acme", "company_id": "co-fpl", "roles": ["customer"], "status": "active", "owner": "Avery"
        }),
    )?;
    put(
        &mut store,
        contacts,
        "c-ada",
        "fungos.crm/contact@1",
        json!({
            "first_name": "Ada", "last_name": "Lovelace", "name": "Ada Lovelace", "email": "ada@acme.example", "contact_token": "t-ada"
        }),
    )?;
    put(
        &mut store,
        crm,
        "ac-a-acme-c-ada",
        "fungos.crm/account-contact@1",
        json!({
            "account_id": "a-acme", "contact_id": "c-ada", "contact_token": "t-ada", "role": "CTO", "status": "active"
        }),
    )?;
    put(
        &mut store,
        crm,
        "proj-acme",
        "fungos.crm/project@1",
        json!({
            "name": "Acme Pilot", "company_id": "co-fpl", "status": "active", "visibility": "team"
        }),
    )?;
    put(
        &mut store,
        crm,
        "pa-proj-acme-a-acme",
        "fungos.crm/project-account@1",
        json!({
            "project_id": "proj-acme", "account_id": "a-acme", "role": "customer", "primary": true
        }),
    )?;
    put(
        &mut store,
        crm,
        "act-1",
        "fungos.crm/activity@1",
        json!({
            "kind": "call", "name": "Discovery call", "occurred_at": "2026-01-03T10:00:00Z",
            "description": "Talked pilot", "account_id": "a-acme", "company_id": "co-fpl",
            "project_id": "proj-acme", "contact_token": "t-ada", "visibility": "team"
        }),
    )?;
    put(
        &mut store,
        crm,
        "opp-1",
        "fungos.crm/opportunity@1",
        json!({
            "name": "Acme pilot", "stage": "proposal", "amount": 50000, "account_id": "a-acme"
        }),
    )?;
    put(
        &mut store,
        messages,
        "thread-acme",
        "fungos.conversation/thread@1",
        json!({
            "channel": "email", "external_id": "thread", "title": "Acme", "participants": ["Ada"],
            "visibility": "team", "first_at_ms": 1, "last_at_ms": 2
        }),
    )?;
    put(
        &mut store,
        messages,
        "m-acme",
        "fungos.conversation/message@1",
        json!({
            "thread_id": "thread-acme", "channel": "email", "external_id": "m", "direction": "inbound",
            "sender": "Ada", "body": "Following up on the pilot", "occurred_at_ms": 1767434400000i64,
            "deleted": false
        }),
    )?;
    put(
        &mut store,
        messages,
        "mention-1",
        "fungos.graph/link",
        json!({
            "from": format!("record:{messages}#m-acme"), "rel": "mentions",
            "to": format!("record:{crm}#a-acme")
        }),
    )?;
    put(
        &mut store,
        calls,
        "call-1",
        "fungos.crm/activity@1",
        json!({
            "kind": "call", "name": "Personal call note", "occurred_at": "2026-01-04T10:00:00Z",
            "description": "Personal record", "account_id": "a-acme", "visibility": "private"
        }),
    )?;
    Ok(())
}

#[test]
fn crm_spore_reads_namespaced_records() -> Result<()> {
    let root = tempfile::tempdir()?;
    seed(root.path())?;
    let records = NamespacedRecordsHost::open(
        root.path(),
        vec![
            Namespace::write("crm", "team/test/crm", "crm"),
            Namespace::read("contacts", "personal/test/contacts", "contact"),
            Namespace::read("messages", "personal/test/messages", "message"),
            Namespace::read("calls", "personal/test/calls", "call"),
        ],
    )?;
    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::new(
        &engine,
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../spores/crm/module.wasm"
        )),
    )?;
    let mut linker = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
    add_to_linker::<Host>(&mut linker)?;
    let mut store = WasmStore::new(
        &engine,
        Host {
            records: Mutex::new(records),
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new().inherit_stdout().build(),
        },
    );
    let bindings = Crm::instantiate(&mut store, &component, &linker)?;
    bindings
        .call_run(&mut store, &["account".into(), "a-acme".into()])?
        .map_err(anyhow::Error::msg)?;
    bindings
        .call_run(
            &mut store,
            &["timeline".into(), "a-acme".into(), "10".into()],
        )?
        .map_err(anyhow::Error::msg)?;
    bindings
        .call_run(&mut store, &["pipeline".into()])?
        .map_err(anyhow::Error::msg)?;
    bindings
        .call_run(&mut store, &["accounts".into()])?
        .map_err(anyhow::Error::msg)?;
    bindings
        .call_run(
            &mut store,
            &[
                "activity".into(),
                "add".into(),
                "act-2".into(),
                "a-acme".into(),
                "meeting".into(),
                "Follow-up".into(),
                "2026-01-05T10:00:00Z".into(),
                "Next step".into(),
            ],
        )?
        .map_err(anyhow::Error::msg)?;
    bindings
        .call_run(
            &mut store,
            &[
                "opportunity".into(),
                "stage".into(),
                "opp-1".into(),
                "won".into(),
            ],
        )?
        .map_err(anyhow::Error::msg)?;
    bindings
        .call_run(
            &mut store,
            &[
                "contact".into(),
                "link".into(),
                "a-acme".into(),
                "c-ada".into(),
                "CTO".into(),
            ],
        )?
        .map_err(anyhow::Error::msg)?;

    let store = Store::open(root.path(), &root.path().join("device_id"))?;
    let activity = store.get("team/test/crm", "act-2")?;
    assert_eq!(activity.schema_id, "fungos.crm/activity@1");
    assert_eq!(activity.fields["name"], "Follow-up");
    assert_eq!(activity.fields["description"], "Next step");
    let opportunity = store.get("team/test/crm", "opp-1")?;
    assert_eq!(opportunity.fields["stage"], "won");
    let link = store.get("team/test/crm", "ac-a-acme-c-ada")?;
    assert_eq!(link.fields["contact_token"], "t-ada");
    Ok(())
}
