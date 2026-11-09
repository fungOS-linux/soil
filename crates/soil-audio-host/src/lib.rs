//! Host implementation of `soil:audio`.
//!
//! The guest writes PCM and names a room. This host turns the name into a real
//! sink: the local device, or an Isochrone destination. The guest never sees an
//! address, a port, an SSRC, or a device path.
//!
//! The room table is a host object. `current` is the room the principal is in;
//! `live` says whether a receiver is attached. Isochrone's route control plane
//! can drive both through a `RoomDirectory` implementation, but nothing in the
//! guest ABI depends on Isochrone being the transport.

use std::sync::Arc;

use wasi_audio_wasmtime::{IsochroneConfig, Output, SinkError, SinkFormat};
use wasmtime::component::{HasSelf, Linker, Resource, ResourceTable};

wasmtime::component::bindgen!({
    path: "wit",
    world: "host",
    with: {
        "soil:audio/audio.output": OutputResource,
    },
});

use soil::audio::audio as api;
pub use soil::audio::audio;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomKind {
    Local,
    Remote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoomTarget {
    Local,
    Remote(IsochroneConfig),
}

/// One room the principal may send to. `live` is the host's belief that a
/// receiver is listening *now*; a room that exists but is not live is refused
/// with `no-listener` rather than played into the void.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    pub name: String,
    pub label: String,
    pub kind: RoomKind,
    pub live: bool,
    pub principal: Option<String>,
    pub target: RoomTarget,
}

pub trait RoomDirectory: Send + Sync {
    fn rooms(&self) -> Vec<Room>;
    fn current(&self) -> Option<String>;
}

/// A fixed room table. Operator config builds this today; a presence adapter
/// (device attached, isochrone route connected) replaces it without changing
/// the WIT or the Spore.
#[derive(Clone, Debug, Default)]
pub struct StaticRooms {
    rooms: Vec<Room>,
    current: Option<String>,
}

impl StaticRooms {
    pub fn new(rooms: Vec<Room>, current: impl Into<Option<String>>) -> Self {
        Self {
            rooms,
            current: current.into(),
        }
    }

    /// The smallest useful host: one local device, and that is the current
    /// room. A machine with no remote routes still has a working soundboard.
    pub fn local() -> Self {
        Self {
            rooms: vec![Room {
                name: "local".into(),
                label: "This machine".into(),
                kind: RoomKind::Local,
                live: true,
                principal: None,
                target: RoomTarget::Local,
            }],
            current: Some("local".into()),
        }
    }

    /// Build a room table from the host environment. This is deliberately
    /// small: one room, one optional Isochrone destination. A presence adapter
    /// replaces it when route discovery exists.
    ///
    /// `FUNGOS_AUDIO_ROOM`   room name (default `local`)
    /// `FUNGOS_AUDIO_DEST`   optional `ip:port`; makes the room remote
    /// `FUNGOS_AUDIO_SSRC`   optional RTP SSRC (default 1399813484)
    /// `FUNGOS_AUDIO_CURRENT` room the principal is in (default the room)
    pub fn from_env() -> Self {
        let name = std::env::var("FUNGOS_AUDIO_ROOM").unwrap_or_else(|_| "local".into());
        let dest = std::env::var("FUNGOS_AUDIO_DEST").ok();
        let ssrc = std::env::var("FUNGOS_AUDIO_SSRC")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(1_399_813_484);
        let current = std::env::var("FUNGOS_AUDIO_CURRENT").unwrap_or_else(|_| name.clone());
        match dest {
            Some(dest) => {
                let destination = match dest.parse() {
                    Ok(destination) => destination,
                    Err(_) => return Self::local(),
                };
                Self {
                    rooms: vec![Room {
                        name: name.clone(),
                        label: name,
                        kind: RoomKind::Remote,
                        live: true,
                        principal: None,
                        target: RoomTarget::Remote(IsochroneConfig {
                            bind: "0.0.0.0:0".parse().expect("valid bind"),
                            destination,
                            ssrc,
                        }),
                    }],
                    current: Some(current),
                }
            }
            None => Self::local(),
        }
    }
}

impl RoomDirectory for StaticRooms {
    fn rooms(&self) -> Vec<Room> {
        self.rooms.clone()
    }

    fn current(&self) -> Option<String> {
        self.current.clone()
    }
}

pub struct OutputResource {
    output: Output,
    room: String,
}

pub struct AudioHost {
    table: ResourceTable,
    rooms: Arc<dyn RoomDirectory>,
}

impl AudioHost {
    pub fn new(rooms: Arc<dyn RoomDirectory>) -> Self {
        Self {
            table: ResourceTable::new(),
            rooms,
        }
    }

    pub fn with_rooms(rooms: impl RoomDirectory + 'static) -> Self {
        Self::new(Arc::new(rooms))
    }
}

/// Resolve a requested room against the host table. `None` and `current` mean
/// the principal's room; a name is an explicit override. Resolution is public
/// so a host can log or test the decision without opening a device.
pub fn resolve(rooms: &dyn RoomDirectory, requested: Option<&str>) -> Result<Room, api::Error> {
    let name = match requested {
        None | Some("current") => rooms.current().ok_or(api::Error::NotReady)?,
        Some(name) => name.to_string(),
    };
    let room = rooms
        .rooms()
        .into_iter()
        .find(|room| room.name == name)
        .ok_or(api::Error::UnknownRoom)?;
    if !room.live {
        return Err(api::Error::NoListener);
    }
    Ok(room)
}

fn map_room(room: Room) -> api::Room {
    api::Room {
        name: room.name,
        label: room.label,
        kind: match room.kind {
            RoomKind::Local => api::RoomKind::Local,
            RoomKind::Remote => api::RoomKind::Remote,
        },
        live: room.live,
        principal: room.principal,
    }
}

fn map_error(error: SinkError) -> api::Error {
    match error {
        SinkError::Denied => api::Error::Denied,
        SinkError::NotReady | SinkError::Underrun => api::Error::NotReady,
        SinkError::OverBudget => api::Error::OverBudget,
        SinkError::Invalid => api::Error::Invalid,
        SinkError::Closed => api::Error::Closed,
        SinkError::Unsupported => api::Error::Unsupported,
        SinkError::Backend => api::Error::Backend,
    }
}

impl api::Host for AudioHost {
    fn open(
        &mut self,
        format: api::Format,
        room: Option<String>,
    ) -> Result<Resource<OutputResource>, api::Error> {
        let room = resolve(self.rooms.as_ref(), room.as_deref())?;
        let isochrone = match &room.target {
            RoomTarget::Local => None,
            RoomTarget::Remote(config) => Some(config.clone()),
        };
        let output = Output::open_sink(
            SinkFormat {
                sample_rate: format.sample_rate,
                channels: format.channels,
            },
            isochrone.as_ref(),
        )
        .map_err(map_error)?;
        self.table
            .push(OutputResource {
                output,
                room: room.name,
            })
            .map_err(|_| api::Error::OverBudget)
    }

    fn rooms(&mut self) -> Result<Vec<api::Room>, api::Error> {
        Ok(self.rooms.rooms().into_iter().map(map_room).collect())
    }

    fn current_room(&mut self) -> Result<Option<api::Room>, api::Error> {
        let Some(name) = self.rooms.current() else {
            return Ok(None);
        };
        Ok(self
            .rooms
            .rooms()
            .into_iter()
            .find(|room| room.name == name)
            .map(map_room))
    }
}

impl api::HostOutput for AudioHost {
    fn format(&mut self, output: Resource<OutputResource>) -> Result<api::Format, api::Error> {
        let output = self.table.get(&output).map_err(|_| api::Error::Closed)?;
        let format = output.output.sink_format();
        Ok(api::Format {
            sample_rate: format.sample_rate,
            channels: format.channels,
        })
    }

    fn write(
        &mut self,
        output: Resource<OutputResource>,
        frames: Vec<f32>,
    ) -> Result<u32, api::Error> {
        let output = self
            .table
            .get_mut(&output)
            .map_err(|_| api::Error::Closed)?;
        output.output.write_sink(&frames).map_err(map_error)
    }

    fn drain(
        &mut self,
        output: Resource<OutputResource>,
        timeout_ms: u32,
    ) -> Result<(), api::Error> {
        let output = self
            .table
            .get_mut(&output)
            .map_err(|_| api::Error::Closed)?;
        output.output.drain_sink(timeout_ms).map_err(map_error)
    }

    fn status(&mut self, output: Resource<OutputResource>) -> Result<api::Status, api::Error> {
        let output = self.table.get(&output).map_err(|_| api::Error::Closed)?;
        let status = output.output.status_sink();
        Ok(api::Status {
            format: api::Format {
                sample_rate: status.format.sample_rate,
                channels: status.format.channels,
            },
            room: output.room.clone(),
            queued_frames: status.queued_frames,
            underruns: status.underruns,
            latency_ms: status.latency_ms,
        })
    }

    fn close(&mut self, output: Resource<OutputResource>) -> Result<(), api::Error> {
        // WIT methods receive a borrowed resource handle. `close` may stop the
        // sink, but it must not remove the entry; only the owned `drop` does
        // that. Deleting here panics the host with `resource.owned()`.
        let output = self
            .table
            .get_mut(&output)
            .map_err(|_| api::Error::Closed)?;
        output.output.close_sink();
        Ok(())
    }

    fn drop(&mut self, output: Resource<OutputResource>) -> Result<(), wasmtime::Error> {
        if let Ok(mut output) = self.table.delete(output) {
            output.output.close_sink();
        }
        Ok(())
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: api::Host + api::HostOutput + 'static,
{
    api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rooms(live: bool, current: Option<&str>) -> StaticRooms {
        StaticRooms::new(
            vec![
                Room {
                    name: "local".into(),
                    label: "This machine".into(),
                    kind: RoomKind::Local,
                    live: true,
                    principal: None,
                    target: RoomTarget::Local,
                },
                Room {
                    name: "home-pi".into(),
                    label: "Home Pi".into(),
                    kind: RoomKind::Remote,
                    live,
                    principal: Some("ajmwagar".into()),
                    target: RoomTarget::Remote(IsochroneConfig {
                        bind: "127.0.0.1:0".parse().unwrap(),
                        destination: "127.0.0.1:50040".parse().unwrap(),
                        ssrc: 1,
                    }),
                },
            ],
            current.map(str::to_string),
        )
    }

    #[test]
    fn current_resolves_to_the_principals_room() {
        let room = resolve(&rooms(true, Some("home-pi")), Some("current")).unwrap();
        assert_eq!(room.name, "home-pi");
    }

    #[test]
    fn none_is_the_host_default_and_equals_current() {
        let room = resolve(&rooms(true, Some("home-pi")), None).unwrap();
        assert_eq!(room.name, "home-pi");
    }

    #[test]
    fn a_named_room_wins_over_current() {
        let room = resolve(&rooms(true, Some("home-pi")), Some("local")).unwrap();
        assert_eq!(room.name, "local");
    }

    #[test]
    fn an_unknown_room_is_named_not_defaulted() {
        assert_eq!(
            resolve(&rooms(true, Some("home-pi")), Some("attic")),
            Err(api::Error::UnknownRoom)
        );
    }

    #[test]
    fn a_silent_room_is_no_listener_not_a_local_fallback() {
        assert_eq!(
            resolve(&rooms(false, Some("home-pi")), Some("home-pi")),
            Err(api::Error::NoListener)
        );
    }

    #[test]
    fn no_current_room_is_not_ready() {
        assert_eq!(
            resolve(&rooms(true, None), Some("current")),
            Err(api::Error::NotReady)
        );
    }

    #[test]
    fn a_local_only_host_still_has_a_working_board() {
        let room = resolve(&StaticRooms::local(), None).unwrap();
        assert_eq!(room.name, "local");
        assert_eq!(room.target, RoomTarget::Local);
    }
}
