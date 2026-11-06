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

wasmtime::component::bindgen!({ path: "../../spores/contacts/wit", world: "contacts" });

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

fn run(bindings: &Contacts, store: &mut WasmStore<Host>, args: &[&str]) -> Result<()> {
    bindings
        .call_run(
            store,
            &args
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>(),
        )?
        .map_err(anyhow::Error::msg)
}

#[test]
fn contacts_spore_edits_and_shares_explicitly() -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut seed = Store::open(root.path(), &root.path().join("device_id"))?;
    seed.put_record(
        "team/test/crm",
        "espo:account:a1",
        "fungos.crm/account@1",
        1,
        json!({"name": "Acme"}),
    )?;
    drop(seed);

    let records = NamespacedRecordsHost::open(
        root.path(),
        vec![
            Namespace::write("contacts", "personal/test/contacts", "contact"),
            Namespace::write("crm:fpl", "team/test/crm", "crm"),
        ],
    )?;
    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::new(
        &engine,
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../spores/contacts/module.wasm"
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
    let bindings = Contacts::instantiate(&mut store, &component, &linker)?;

    let ns = ["--contacts", "contacts", "--crm", "crm:fpl"];
    let mut args = ns.to_vec();
    args.extend([
        "add",
        "c-ada",
        "Ada",
        "Lovelace",
        "ada@acme.example",
        "+1 555 0101",
        "CTO",
    ]);
    run(&bindings, &mut store, &args)?;

    let mut args = ns.to_vec();
    args.extend(["set", "c-ada", "title", "VP Engineering"]);
    run(&bindings, &mut store, &args)?;

    let mut args = ns.to_vec();
    args.extend(["share", "espo:account:a1", "c-ada", "Ada"]);
    run(&bindings, &mut store, &args)?;

    let mut args = ns.to_vec();
    args.extend(["shared"]);
    run(&bindings, &mut store, &args)?;

    let mut args = ns.to_vec();
    args.extend(["view", "c-ada"]);
    run(&bindings, &mut store, &args)?;

    let check = Store::open(root.path(), &root.path().join("device_id"))?;
    assert_eq!(
        check
            .get("team/test/crm", "proj-espo:account:a1-c-ada")?
            .schema_id,
        "fungos.crm/account-contact-projection@1"
    );
    drop(check);

    let mut args = ns.to_vec();
    args.extend(["show", "c-ada"]);
    run(&bindings, &mut store, &args)?;

    let mut args = ns.to_vec();
    args.extend(["unshare", "espo:account:a1", "c-ada"]);
    run(&bindings, &mut store, &args)?;

    let store = Store::open(root.path(), &root.path().join("device_id"))?;
    let contact = store.get("personal/test/contacts", "c-ada")?;
    assert_eq!(contact.fields["title"], "VP Engineering");
    assert_eq!(contact.fields["uid"], "ct-c-ada");
    assert!(
        store
            .get("team/test/crm", "proj-espo:account:a1-c-ada")
            .map(|record| record.deleted)
            .unwrap_or(true)
    );
    let audits = store.list_schema("team/test/crm", "fungos.audit/event@1", 100)?;
    assert!(
        audits
            .iter()
            .any(|record| record.fields["action"] == "crm.contact.share")
    );
    assert!(
        audits
            .iter()
            .any(|record| record.fields["action"] == "crm.contact.unshare")
    );
    Ok(())
}
