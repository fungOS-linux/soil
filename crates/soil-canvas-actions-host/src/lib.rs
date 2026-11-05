//! Host binding for the `fungos:actions/catalog@1.0.0` presentation projection.
//!
//! Canvas exposes the granted action catalog to a Spore. The host owns grants,
//! approval, idempotency and routing; this adapter only projects the catalog.
//! It never invokes an action.

use std::sync::Arc;

use anyhow::Result;
use soil_actions_host::{ActionHost, CanvasAction, Effect, Idempotency};
use wasmtime::component::{Component, HasSelf, Linker, ResourceTable};
use wasmtime::{Config, Engine, Store};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({ path: "wit", world: "actions-guest" });

use fungos::actions::catalog::{
    self, Action, Approval, Effect as WitEffect, Idempotency as WitIdempotency,
};

pub struct CanvasActionsRuntime {
    engine: Engine,
    linker: Linker<CanvasActionsHost>,
}

impl CanvasActionsRuntime {
    pub fn new() -> Result<Self> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(anyhow::Error::from)?;
        add_to_linker(&mut linker)?;
        Ok(Self { engine, linker })
    }

    /// Run a component that imports `fungos:actions/catalog` and exports
    /// `run() -> result<u32, string>`.
    pub fn run(&self, bytes: &[u8], host: CanvasActionsHost) -> Result<u32> {
        let component = Component::new(&self.engine, bytes)?;
        let mut store = Store::new(&self.engine, host);
        let bindings = ActionsGuest::instantiate(&mut store, &component, &self.linker)?;
        let count = bindings.call_run(&mut store)?.map_err(anyhow::Error::msg)?;
        Ok(count)
    }
}

pub struct CanvasActionsHost {
    actions: Arc<ActionHost>,
    table: ResourceTable,
    wasi: WasiCtx,
}

impl CanvasActionsHost {
    pub fn new(actions: Arc<ActionHost>) -> Self {
        Self {
            actions,
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new()
                .inherit_stdout()
                .inherit_stderr()
                .build(),
        }
    }

    pub fn actions(&self) -> &ActionHost {
        &self.actions
    }

    pub fn projected(&self) -> Vec<CanvasAction> {
        self.actions.canvas_actions()
    }
}

impl WasiView for CanvasActionsHost {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl catalog::Host for CanvasActionsHost {
    fn list_actions(&mut self) -> Result<Vec<Action>, catalog::Error> {
        Ok(self
            .actions
            .canvas_actions()
            .into_iter()
            .map(project)
            .collect())
    }
}

/// Add the Canvas action catalog import to a component linker.
pub fn add_to_linker(linker: &mut Linker<CanvasActionsHost>) -> Result<()> {
    catalog::add_to_linker::<CanvasActionsHost, HasSelf<CanvasActionsHost>>(linker, |host| host)?;
    Ok(())
}

fn project(action: CanvasAction) -> Action {
    Action {
        id: action.id,
        label: action.label,
        description: action.description,
        shortcut: action.shortcut,
        input_schema: action.input_schema,
        requires_input: action.requires_input,
        effect: match action.effect {
            Effect::Read => WitEffect::Read,
            Effect::Write => WitEffect::Write,
            Effect::External => WitEffect::External,
        },
        idempotency: match action.idempotency {
            Idempotency::Idempotent => WitIdempotency::Idempotent,
            Idempotency::Keyed => WitIdempotency::Keyed,
            Idempotency::Unsafe => WitIdempotency::Unsafe,
        },
        approval: match action.confirmation {
            soil_actions_host::Approval::None => Approval::None,
            soil_actions_host::Approval::Operator => Approval::Operator,
            soil_actions_host::Approval::Destructive => Approval::Destructive,
        },
        available: action.available,
        reason: action.reason,
        phases: action.phases,
        provider: action.provider,
        catalog_revision: action.catalog_revision,
        state_revision: action.state_revision,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use soil_actions_host::{
        ActionCall, ActionDescriptor, ActionError, ActionGrants, ActionRouter, IdempotencyStore,
    };

    use super::*;
    use catalog::Host as _;

    struct NoopRouter;

    impl ActionRouter for NoopRouter {
        fn invoke(&self, _: &ActionDescriptor, _: &ActionCall) -> Result<String, ActionError> {
            Ok("{}".into())
        }
    }

    fn descriptor(id: &str, schema: &str) -> ActionDescriptor {
        ActionDescriptor {
            id: id.into(),
            version: 1,
            summary: id.into(),
            effect: Effect::Write,
            idempotency: Idempotency::Idempotent,
            approval: soil_actions_host::Approval::None,
            input_schema: schema.into(),
            output_schema: "{}".into(),
            phases: vec!["commit".into()],
            provider: Some("test".into()),
            catalog_revision: Some("cat-1".into()),
            state_revision: Some("state-1".into()),
        }
    }

    #[test]
    fn list_actions_projects_click_and_parameter_actions() {
        let mut grants = ActionGrants::default();
        grants.allow("lights.on", 1);
        grants.allow("lights.level", 1);
        let host = ActionHost::new(
            [
                descriptor("lights.on", "{}"),
                descriptor(
                    "lights.level",
                    r#"{"type":"object","properties":{"level":{"type":"integer","minimum":0,"maximum":100}}}"#,
                ),
            ],
            grants,
            Arc::new(NoopRouter),
            IdempotencyStore::in_memory().unwrap(),
        )
        .unwrap();
        let mut canvas = CanvasActionsHost::new(Arc::new(host));
        let listed = canvas.list_actions().unwrap();
        assert_eq!(listed.len(), 2);
        let click = listed
            .iter()
            .find(|action| action.id == "lights.on")
            .unwrap();
        let parameter = listed
            .iter()
            .find(|action| action.id == "lights.level")
            .unwrap();
        assert!(!click.requires_input);
        assert!(parameter.requires_input);
        assert_eq!(parameter.catalog_revision.as_deref(), Some("cat-1"));
        assert_eq!(parameter.phases, vec!["commit"]);
    }

    #[test]
    fn guest_lists_actions_through_the_canvas_binding() {
        let mut grants = ActionGrants::default();
        grants.allow("lights.on", 1);
        grants.allow("lights.level", 1);
        let host = ActionHost::new(
            [
                descriptor("lights.on", "{}"),
                descriptor(
                    "lights.level",
                    r#"{"type":"object","properties":{"level":{"type":"integer","minimum":0,"maximum":100}}}"#,
                ),
            ],
            grants,
            Arc::new(NoopRouter),
            IdempotencyStore::in_memory().unwrap(),
        )
        .unwrap();
        let runtime = CanvasActionsRuntime::new().unwrap();
        let count = runtime
            .run(
                include_bytes!("../fixtures/list-actions.wasm"),
                CanvasActionsHost::new(Arc::new(host)),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn empty_grant_catalog_is_empty() {
        let host = ActionHost::new(
            [descriptor("lights.on", "{}")],
            ActionGrants::default(),
            Arc::new(NoopRouter),
            IdempotencyStore::in_memory().unwrap(),
        )
        .unwrap();
        let mut canvas = CanvasActionsHost::new(Arc::new(host));
        assert!(canvas.list_actions().unwrap().is_empty());
        let _ = BTreeSet::<String>::new();
    }
}
