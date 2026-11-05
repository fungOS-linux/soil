//! Wasmtime component bindings for the notify and ringer host.

use std::sync::atomic::{AtomicU64, Ordering};

use wasmtime::component::{HasSelf, Linker, Resource, ResourceTable};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::{
    Action, CleanupScope, Context, Envelope, Filter, FocusProfile, NotifyBroker, NotifyError,
    Priority, RingPhase, RingState,
};

wasmtime::component::bindgen!({
    path: "wit",
    world: "runner",
    with: {
        "soil:notify/inbox.subscription": InboxSubscription,
        "soil:ringer/calls.subscription": RingerSubscription,
    },
});

use soil::notify::{
    cleanup as cleanup_api, focus as focus_api, history as history_api, inbox as inbox_api,
    publisher as publisher_api, types as notify_types,
};
use soil::ringer::calls as ringer_api;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct NotifyComponent {
    broker: NotifyBroker,
    app_id: String,
    table: ResourceTable,
    wasi: WasiCtx,
}

pub struct InboxSubscription {
    cursor: u64,
}

pub struct RingerSubscription {
    sent: bool,
}

impl NotifyComponent {
    pub fn new(broker: NotifyBroker, app_id: impl Into<String>) -> Self {
        Self {
            broker,
            app_id: app_id.into(),
            table: ResourceTable::new(),
            wasi: WasiCtxBuilder::new()
                .inherit_stdout()
                .inherit_stderr()
                .build(),
        }
    }

    pub fn broker(&self) -> &NotifyBroker {
        &self.broker
    }

    pub fn with_peers(mut self, peers: impl IntoIterator<Item = String>) -> Self {
        self.broker.set_peers(peers);
        self
    }

    fn next_id(&self) -> String {
        let seq = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        format!("{}:{}:{}", self.app_id, crate::now_ms(), seq)
    }
}

fn map_action(action: notify_types::Action) -> Action {
    Action {
        id: action.id,
        label: action.label,
        destructive: action.destructive,
    }
}

fn map_context(context: notify_types::Context) -> Context {
    Context {
        summary: context.summary,
        entities: context.entities,
        topics: context.topics,
        urgency_hint: context.urgency_hint.map(map_priority),
    }
}

fn map_priority(priority: notify_types::Priority) -> Priority {
    match priority {
        notify_types::Priority::Passive => Priority::Passive,
        notify_types::Priority::Active => Priority::Active,
        notify_types::Priority::Urgent => Priority::Urgent,
    }
}

fn api_priority(priority: Priority) -> notify_types::Priority {
    match priority {
        Priority::Passive => notify_types::Priority::Passive,
        Priority::Active => notify_types::Priority::Active,
        Priority::Urgent => notify_types::Priority::Urgent,
    }
}

fn api_action(action: &Action) -> notify_types::Action {
    notify_types::Action {
        id: action.id.clone(),
        label: action.label.clone(),
        destructive: action.destructive,
    }
}

fn notify_error(error: NotifyError) -> notify_types::Error {
    match error {
        NotifyError::Denied => notify_types::Error::Denied,
        NotifyError::NotFound => notify_types::Error::NotReady,
        NotifyError::NotRinging | NotifyError::AlreadyAnswered => notify_types::Error::Stale,
        NotifyError::Invalid(_) => notify_types::Error::Invalid,
        NotifyError::Io(_) => notify_types::Error::Io,
    }
}

fn ringer_error(error: NotifyError) -> ringer_api::Error {
    match error {
        NotifyError::Denied => ringer_api::Error::Denied,
        NotifyError::NotFound => ringer_api::Error::NotFound,
        NotifyError::NotRinging => ringer_api::Error::NotRinging,
        NotifyError::AlreadyAnswered => ringer_api::Error::AlreadyAnswered,
        NotifyError::Invalid(_) => ringer_api::Error::Invalid,
        NotifyError::Io(_) => ringer_api::Error::Io,
    }
}

fn envelope_from_notification(
    id: String,
    app_id: &str,
    source_device: &str,
    notification: publisher_api::Notification,
) -> Envelope {
    Envelope {
        id,
        app_id: app_id.to_owned(),
        source_device: source_device.to_owned(),
        title: notification.title,
        body: notification.body,
        category: notification.category,
        thread: notification.thread,
        group_key: notification.group_key,
        created_at_ms: crate::now_ms(),
        expires_at_ms: notification.expires_at_ms,
        priority: notification
            .priority_hint
            .map(map_priority)
            .unwrap_or(Priority::Passive),
        sensitive: notification.sensitive,
        actions: notification.actions.into_iter().map(map_action).collect(),
        context: notification.context.map(map_context),
    }
}

fn api_item(item: crate::InboxItem) -> inbox_api::Item {
    inbox_api::Item {
        id: item.envelope.id,
        app_id: item.envelope.app_id,
        title: item.envelope.title,
        body: item.envelope.body,
        category: item.envelope.category,
        thread: item.envelope.thread,
        group_key: item.envelope.group_key,
        priority: api_priority(item.envelope.priority),
        posted_at_ms: item.envelope.created_at_ms,
        read: item.read,
        held: item.held,
        sensitive: item.envelope.sensitive,
        actions: item.envelope.actions.iter().map(api_action).collect(),
        summary: item.envelope.context.and_then(|context| context.summary),
    }
}

fn api_focus(profile: FocusProfile) -> focus_api::Profile {
    focus_api::Profile {
        mode: profile.mode,
        until_ms: profile.until_ms,
        allow_apps: profile.allow_apps,
        allow_categories: profile.allow_categories,
        allow_threads: profile.allow_threads,
        breakthrough: profile.breakthrough,
        rollup_held: profile.rollup_held,
    }
}

fn map_focus(profile: focus_api::Profile) -> FocusProfile {
    FocusProfile {
        mode: profile.mode,
        until_ms: profile.until_ms,
        allow_apps: profile.allow_apps,
        allow_categories: profile.allow_categories,
        allow_threads: profile.allow_threads,
        breakthrough: profile.breakthrough,
        rollup_held: profile.rollup_held,
    }
}

fn map_cleanup(scope: cleanup_api::Scope) -> CleanupScope {
    match scope {
        cleanup_api::Scope::Read => CleanupScope::Read,
        cleanup_api::Scope::Shown => CleanupScope::Shown,
        cleanup_api::Scope::Held => CleanupScope::Held,
        cleanup_api::Scope::All => CleanupScope::All,
        cleanup_api::Scope::OlderThan => CleanupScope::OlderThan,
        cleanup_api::Scope::App => CleanupScope::App,
        cleanup_api::Scope::Category => CleanupScope::Category,
        cleanup_api::Scope::Archive => CleanupScope::Archive,
    }
}

fn api_ring_state(state: RingState) -> ringer_api::State {
    ringer_api::State {
        id: state.id,
        phase: match state.phase {
            RingPhase::Ringing => ringer_api::Phase::Ringing,
            RingPhase::Accepted => ringer_api::Phase::Accepted,
            RingPhase::Declined => ringer_api::Phase::Declined,
            RingPhase::Ended => ringer_api::Phase::Ended,
            RingPhase::Missed => ringer_api::Phase::Missed,
        },
        answered_by: state.answered_by,
    }
}

impl WasiView for NotifyComponent {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl publisher_api::Host for NotifyComponent {
    fn publish(
        &mut self,
        notification: publisher_api::Notification,
    ) -> Result<String, notify_types::Error> {
        let id = self.next_id();
        let envelope = envelope_from_notification(
            id.clone(),
            &self.app_id,
            self.broker.device_id(),
            notification,
        );
        self.broker.publish(envelope).map_err(notify_error)?;
        Ok(id)
    }

    fn update(
        &mut self,
        id: String,
        notification: publisher_api::Notification,
    ) -> Result<(), notify_types::Error> {
        let envelope = envelope_from_notification(
            id.clone(),
            &self.app_id,
            self.broker.device_id(),
            notification,
        );
        self.broker.update(&id, &envelope).map_err(notify_error)
    }

    fn withdraw(&mut self, id: String) -> Result<(), notify_types::Error> {
        self.broker
            .withdraw(&self.app_id, &id)
            .map_err(notify_error)
    }
}

impl inbox_api::Host for NotifyComponent {
    fn query(&mut self, filter: inbox_api::Filter) -> Result<inbox_api::Page, notify_types::Error> {
        let filter = Filter {
            apps: filter.apps,
            categories: filter.categories,
            include_held: filter.include_held,
            include_read: filter.include_read,
            offset: filter.offset,
            limit: filter.limit,
        };
        let items = self.broker.inbox(&filter).map_err(notify_error)?;
        let count = items.len() as u64;
        let next =
            (count >= u64::from(filter.limit) && filter.limit > 0).then_some(filter.offset + count);
        Ok(inbox_api::Page {
            items: items.into_iter().map(api_item).collect(),
            next,
        })
    }

    fn subscribe(&mut self) -> Result<Resource<InboxSubscription>, notify_types::Error> {
        self.table
            .push(InboxSubscription { cursor: 0 })
            .map_err(|_| notify_types::Error::OverBudget)
    }

    fn dismiss(&mut self, id: String) -> Result<(), notify_types::Error> {
        self.broker.dismiss(&id).map_err(notify_error)
    }

    fn act(&mut self, id: String, action_id: String) -> Result<(), notify_types::Error> {
        self.broker.act(&id, &action_id).map_err(notify_error)
    }
}

impl inbox_api::HostSubscription for NotifyComponent {
    fn next(
        &mut self,
        subscription: Resource<InboxSubscription>,
        _timeout_ms: u32,
    ) -> Result<Option<inbox_api::Item>, notify_types::Error> {
        let subscription = self
            .table
            .get_mut(&subscription)
            .map_err(|_| notify_types::Error::Stale)?;
        let items = self
            .broker
            .inbox(&Filter {
                include_held: true,
                include_read: true,
                offset: subscription.cursor,
                limit: 1,
                ..Filter::default()
            })
            .map_err(notify_error)?;
        if let Some(item) = items.into_iter().next() {
            subscription.cursor += 1;
            Ok(Some(api_item(item)))
        } else {
            Ok(None)
        }
    }

    fn drop(&mut self, subscription: Resource<InboxSubscription>) -> Result<(), wasmtime::Error> {
        let _ = self.table.delete(subscription);
        Ok(())
    }
}

impl focus_api::Host for NotifyComponent {
    fn get(&mut self) -> Result<Option<focus_api::Profile>, notify_types::Error> {
        self.broker
            .focus()
            .map(|profile| profile.map(api_focus))
            .map_err(notify_error)
    }

    fn set(&mut self, profile: focus_api::Profile) -> Result<(), notify_types::Error> {
        self.broker
            .set_focus(map_focus(profile))
            .map_err(notify_error)
    }

    fn clear(&mut self) -> Result<(), notify_types::Error> {
        self.broker.clear_focus().map_err(notify_error)
    }
}

impl history_api::Host for NotifyComponent {
    fn query(&mut self, offset: u64, limit: u32) -> Result<history_api::Page, notify_types::Error> {
        let entries = self.broker.history(offset, limit).map_err(notify_error)?;
        let count = entries.len() as u64;
        let next = (count >= u64::from(limit) && limit > 0).then_some(offset + count);
        Ok(history_api::Page {
            entries: entries
                .into_iter()
                .map(|entry| history_api::Entry {
                    id: entry.id,
                    app_id: entry.app_id,
                    title: entry.title,
                    body: entry.body,
                    priority: api_priority(entry.priority),
                    posted_at_ms: entry.created_at_ms,
                    dismissed_at_ms: None,
                })
                .collect(),
            next,
        })
    }
}

impl cleanup_api::Host for NotifyComponent {
    fn clear(
        &mut self,
        scope: cleanup_api::Scope,
        value: Option<String>,
        older_than_ms: Option<u64>,
    ) -> Result<u32, notify_types::Error> {
        self.broker
            .cleanup(map_cleanup(scope), value.as_deref(), older_than_ms)
            .map(|receipt| receipt.cleared)
            .map_err(notify_error)
    }
}

impl ringer_api::Host for NotifyComponent {
    fn incoming(&mut self) -> Result<Resource<RingerSubscription>, ringer_api::Error> {
        self.table
            .push(RingerSubscription { sent: false })
            .map_err(|_| ringer_api::Error::OverBudget)
    }

    fn accept(&mut self, id: String) -> Result<ringer_api::State, ringer_api::Error> {
        self.broker
            .accept_ring(&id)
            .map(api_ring_state)
            .map_err(ringer_error)
    }

    fn decline(&mut self, id: String) -> Result<ringer_api::State, ringer_api::Error> {
        self.broker
            .decline_ring(&id)
            .map(api_ring_state)
            .map_err(ringer_error)
    }

    fn end(&mut self, id: String) -> Result<ringer_api::State, ringer_api::Error> {
        self.broker
            .end_ring(&id)
            .map(api_ring_state)
            .map_err(ringer_error)
    }

    fn mute(&mut self, id: String) -> Result<(), ringer_api::Error> {
        self.broker.mute_ring(&id).map_err(ringer_error)
    }

    fn get_state(&mut self, id: String) -> Result<ringer_api::State, ringer_api::Error> {
        self.broker
            .ring_state(&id)
            .map(api_ring_state)
            .map_err(ringer_error)
    }
}

impl ringer_api::HostSubscription for NotifyComponent {
    fn next(
        &mut self,
        subscription: Resource<RingerSubscription>,
        _timeout_ms: u32,
    ) -> Result<Option<ringer_api::State>, ringer_api::Error> {
        let subscription = self
            .table
            .get_mut(&subscription)
            .map_err(|_| ringer_api::Error::Unavailable)?;
        if subscription.sent {
            return Ok(None);
        }
        let state = self.broker.active_ring().map_err(ringer_error)?;
        if let Some(state) = state {
            subscription.sent = true;
            Ok(Some(api_ring_state(state)))
        } else {
            Ok(None)
        }
    }

    fn drop(&mut self, subscription: Resource<RingerSubscription>) -> Result<(), wasmtime::Error> {
        let _ = self.table.delete(subscription);
        Ok(())
    }
}

pub fn add_to_linker(linker: &mut Linker<NotifyComponent>) -> anyhow::Result<()> {
    publisher_api::add_to_linker::<NotifyComponent, HasSelf<NotifyComponent>>(linker, |host| host)?;
    inbox_api::add_to_linker::<NotifyComponent, HasSelf<NotifyComponent>>(linker, |host| host)?;
    focus_api::add_to_linker::<NotifyComponent, HasSelf<NotifyComponent>>(linker, |host| host)?;
    history_api::add_to_linker::<NotifyComponent, HasSelf<NotifyComponent>>(linker, |host| host)?;
    cleanup_api::add_to_linker::<NotifyComponent, HasSelf<NotifyComponent>>(linker, |host| host)?;
    ringer_api::add_to_linker::<NotifyComponent, HasSelf<NotifyComponent>>(linker, |host| host)?;
    Ok(())
}

/// Run the notify Spore against this host. The Spore still sees only its
/// granted capability imports, never Unibus, Iroh, paths or sockets.
pub struct NotifyRuntime {
    engine: wasmtime::Engine,
    linker: Linker<NotifyComponent>,
}

impl NotifyRuntime {
    pub fn new() -> anyhow::Result<Self> {
        let mut config = wasmtime::Config::new();
        config.wasm_component_model(true);
        let engine = wasmtime::Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        wasmtime_wasi::p2::add_to_linker_sync(&mut linker).map_err(anyhow::Error::from)?;
        add_to_linker(&mut linker)?;
        Ok(Self { engine, linker })
    }

    pub fn run_component(
        &self,
        bytes: &[u8],
        host: NotifyComponent,
        args: &[String],
    ) -> anyhow::Result<()> {
        let component = wasmtime::component::Component::new(&self.engine, bytes)?;
        let mut store = wasmtime::Store::new(&self.engine, host);
        let bindings = Runner::instantiate(&mut store, &component, &self.linker)?;
        bindings
            .call_run(&mut store, args)?
            .map_err(|error| anyhow::anyhow!("guest: {error}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Context};

    #[test]
    fn runs_the_real_notify_spore_against_the_host() {
        let broker = NotifyBroker::in_memory("mac").unwrap();
        broker
            .publish(Envelope {
                id: "seed-1".into(),
                app_id: "dev.fungos.soil".into(),
                source_device: "mac".into(),
                title: "Build failed".into(),
                body: "soil-notify-host".into(),
                category: "build".into(),
                thread: None,
                group_key: None,
                created_at_ms: crate::now_ms(),
                expires_at_ms: None,
                priority: Priority::Active,
                sensitive: false,
                actions: vec![Action {
                    id: "open".into(),
                    label: "Open".into(),
                    destructive: false,
                }],
                context: Some(Context {
                    summary: Some("CI failed".into()),
                    ..Context::default()
                }),
            })
            .unwrap();
        let host = NotifyComponent::new(broker, "dev.fungos.notify");
        let runtime = NotifyRuntime::new().unwrap();
        runtime
            .run_component(
                include_bytes!("../../../spores/notify/module.wasm"),
                host,
                &["inbox".into()],
            )
            .unwrap();
    }
}
