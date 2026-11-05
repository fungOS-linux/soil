//! Host implementation of `soil:media`.
//!
//! The host owns source resolution, credentials, transport, decode, and
//! presentation. A Spore chooses a reviewed source id and playback controls;
//! encoded access and image readback are separate interfaces.

use std::sync::Arc;
use std::time::Duration;

use image::{DynamicImage, GenericImageView};
use wasmtime::component::{HasSelf, Linker, Resource, ResourceTable};

wasmtime::component::bindgen!({
    path: "wit",
    world: "host",
    with: {
        "soil:media/playback.playback": PlaybackResource,
        "soil:media/encoded.encoded-stream": EncodedResource,
        "soil:media/image.image": ImageResource,
    },
});

use soil::media::{encoded as encoded_api, image as image_api, playback as api};

const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 8192;
const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    Denied,
    UnknownSource,
    NotReady,
    Unsupported,
    Decode,
    OverBudget,
    Invalid,
    Closed,
}

#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    pub start_ms: Option<u64>,
    pub audio: bool,
    pub video: bool,
}

#[derive(Debug, Clone)]
pub struct Format {
    pub container: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    Idle,
    Opening,
    Buffering,
    Playing,
    Paused,
    Ended,
}

#[derive(Debug, Clone)]
pub struct PlaybackStatus {
    pub state: PlaybackState,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub buffered_ms: Option<u64>,
    pub dropped_frames: u32,
}

#[derive(Debug, Clone)]
pub enum PlaybackEvent {
    Opened(Format),
    Buffering,
    Playing,
    Paused,
    Position(u64),
    FormatChanged(Format),
    Discontinuity,
    Ended,
}

#[derive(Debug, Clone)]
pub struct EncodedChunk {
    pub data: Vec<u8>,
    pub pts_ms: u64,
    pub duration_ms: Option<u64>,
    pub keyframe: bool,
    pub codec_config: Option<Vec<u8>>,
    pub discontinuity: bool,
}

/// A host-side source adapter. Jellyfin, YouTube, Unibus/CID, local files, or a
/// platform media player implement this trait outside the guest ABI.
pub trait MediaSource: Send + Sync {
    fn open_playback(
        &self,
        _source_id: &str,
        _options: OpenOptions,
    ) -> Result<Box<dyn PlaybackSession>, MediaError> {
        Err(MediaError::Unsupported)
    }

    fn open_encoded(&self, _source_id: &str) -> Result<Box<dyn EncodedSession>, MediaError> {
        Err(MediaError::Unsupported)
    }

    fn read_image(&self, _source_id: &str) -> Result<Vec<u8>, MediaError> {
        Err(MediaError::Unsupported)
    }
}

pub trait PlaybackSession: Send {
    fn status(&mut self) -> Result<PlaybackStatus, MediaError>;
    fn next(&mut self, timeout: Duration) -> Result<Option<PlaybackEvent>, MediaError>;
    fn play(&mut self) -> Result<(), MediaError>;
    fn pause(&mut self) -> Result<(), MediaError>;
    fn seek(&mut self, position_ms: u64) -> Result<(), MediaError>;
    fn close(&mut self) -> Result<(), MediaError>;
}

pub trait EncodedSession: Send {
    fn next(&mut self, timeout: Duration) -> Result<Option<EncodedChunk>, MediaError>;
    fn close(&mut self) -> Result<(), MediaError>;
}

pub struct PlaybackResource {
    session: Box<dyn PlaybackSession>,
    closed: bool,
}

pub struct EncodedResource {
    session: Box<dyn EncodedSession>,
    closed: bool,
}

pub struct ImageResource {
    info: image_api::Info,
    image: DynamicImage,
    closed: bool,
}

pub struct MediaHost {
    table: ResourceTable,
    source: Arc<dyn MediaSource>,
}

impl MediaHost {
    pub fn new(source: Arc<dyn MediaSource>) -> Self {
        Self {
            table: ResourceTable::new(),
            source,
        }
    }

    pub fn with_source(source: impl MediaSource + 'static) -> Self {
        Self::new(Arc::new(source))
    }
}

impl api::Host for MediaHost {
    fn open(
        &mut self,
        source_id: String,
        options: api::OpenOptions,
    ) -> Result<Resource<PlaybackResource>, api::Error> {
        let options = OpenOptions {
            start_ms: options.start_ms,
            audio: options.audio,
            video: options.video,
        };
        let session = self
            .source
            .open_playback(&source_id, options)
            .map_err(playback_error)?;
        self.table
            .push(PlaybackResource {
                session,
                closed: false,
            })
            .map_err(|_| api::Error::OverBudget)
    }
}

impl api::HostPlayback for MediaHost {
    fn status(&mut self, playback: Resource<PlaybackResource>) -> Result<api::Status, api::Error> {
        let playback = self
            .table
            .get_mut(&playback)
            .map_err(|_| api::Error::Closed)?;
        if playback.closed {
            return Err(api::Error::Closed);
        }
        playback
            .session
            .status()
            .map(status)
            .map_err(playback_error)
    }

    fn next(
        &mut self,
        playback: Resource<PlaybackResource>,
        timeout_ms: u32,
    ) -> Result<Option<api::Event>, api::Error> {
        let playback = self
            .table
            .get_mut(&playback)
            .map_err(|_| api::Error::Closed)?;
        if playback.closed {
            return Err(api::Error::Closed);
        }
        playback
            .session
            .next(Duration::from_millis(u64::from(timeout_ms)))
            .map(|event| event.map(api_event))
            .map_err(playback_error)
    }

    fn play(&mut self, playback: Resource<PlaybackResource>) -> Result<(), api::Error> {
        with_playback(&mut self.table, &playback, |session| session.play())
    }

    fn pause(&mut self, playback: Resource<PlaybackResource>) -> Result<(), api::Error> {
        with_playback(&mut self.table, &playback, |session| session.pause())
    }

    fn seek(
        &mut self,
        playback: Resource<PlaybackResource>,
        position_ms: u64,
    ) -> Result<(), api::Error> {
        with_playback(&mut self.table, &playback, |session| {
            session.seek(position_ms)
        })
    }

    fn close(&mut self, playback: Resource<PlaybackResource>) -> Result<(), api::Error> {
        let mut playback = self
            .table
            .delete(playback)
            .map_err(|_| api::Error::Closed)?;
        if !playback.closed {
            playback.closed = true;
            playback.session.close().map_err(playback_error)?;
        }
        Ok(())
    }

    fn drop(&mut self, playback: Resource<PlaybackResource>) -> Result<(), wasmtime::Error> {
        let Ok(mut playback) = self.table.delete(playback) else {
            return Ok(());
        };
        if !playback.closed {
            playback.closed = true;
            let _ = playback.session.close();
        }
        Ok(())
    }
}

fn with_playback(
    table: &mut ResourceTable,
    resource: &Resource<PlaybackResource>,
    call: impl FnOnce(&mut dyn PlaybackSession) -> Result<(), MediaError>,
) -> Result<(), api::Error> {
    let playback = table.get_mut(resource).map_err(|_| api::Error::Closed)?;
    if playback.closed {
        return Err(api::Error::Closed);
    }
    call(playback.session.as_mut()).map_err(playback_error)
}

impl encoded_api::Host for MediaHost {
    fn open(&mut self, source_id: String) -> Result<Resource<EncodedResource>, encoded_api::Error> {
        let session = self
            .source
            .open_encoded(&source_id)
            .map_err(encoded_error)?;
        self.table
            .push(EncodedResource {
                session,
                closed: false,
            })
            .map_err(|_| encoded_api::Error::OverBudget)
    }
}

impl encoded_api::HostEncodedStream for MediaHost {
    fn next(
        &mut self,
        stream: Resource<EncodedResource>,
        timeout_ms: u32,
    ) -> Result<Option<encoded_api::Chunk>, encoded_api::Error> {
        let stream = self
            .table
            .get_mut(&stream)
            .map_err(|_| encoded_api::Error::Closed)?;
        if stream.closed {
            return Err(encoded_api::Error::Closed);
        }
        stream
            .session
            .next(Duration::from_millis(u64::from(timeout_ms)))
            .map(|chunk| chunk.map(encoded_chunk))
            .map_err(encoded_error)
    }

    fn close(&mut self, stream: Resource<EncodedResource>) -> Result<(), encoded_api::Error> {
        let mut stream = self
            .table
            .delete(stream)
            .map_err(|_| encoded_api::Error::Closed)?;
        if !stream.closed {
            stream.closed = true;
            stream.session.close().map_err(encoded_error)?;
        }
        Ok(())
    }

    fn drop(&mut self, stream: Resource<EncodedResource>) -> Result<(), wasmtime::Error> {
        let Ok(mut stream) = self.table.delete(stream) else {
            return Ok(());
        };
        if !stream.closed {
            stream.closed = true;
            let _ = stream.session.close();
        }
        Ok(())
    }
}

impl image_api::Host for MediaHost {
    fn open(&mut self, source_id: String) -> Result<Resource<ImageResource>, image_api::Error> {
        let bytes = self.source.read_image(&source_id).map_err(image_error)?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err(image_api::Error::OverBudget);
        }
        let media_type = image::guess_format(&bytes)
            .map_err(|_| image_api::Error::Invalid)?
            .to_mime_type()
            .to_string();
        let image = image::load_from_memory(&bytes).map_err(|_| image_api::Error::Invalid)?;
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 {
            return Err(image_api::Error::Invalid);
        }
        self.table
            .push(ImageResource {
                info: image_api::Info {
                    width,
                    height,
                    media_type,
                },
                image,
                closed: false,
            })
            .map_err(|_| image_api::Error::OverBudget)
    }
}

impl image_api::HostImage for MediaHost {
    fn info(
        &mut self,
        image: Resource<ImageResource>,
    ) -> Result<image_api::Info, image_api::Error> {
        let image = self
            .table
            .get(&image)
            .map_err(|_| image_api::Error::Closed)?;
        if image.closed {
            return Err(image_api::Error::Closed);
        }
        Ok(image.info.clone())
    }

    fn rgba(
        &mut self,
        image: Resource<ImageResource>,
        max_width: u32,
        max_height: u32,
    ) -> Result<image_api::Rgba, image_api::Error> {
        let image = self
            .table
            .get_mut(&image)
            .map_err(|_| image_api::Error::Closed)?;
        if image.closed {
            return Err(image_api::Error::Closed);
        }
        let max_width = max_width.clamp(1, MAX_IMAGE_DIMENSION);
        let max_height = max_height.clamp(1, MAX_IMAGE_DIMENSION);
        let (width, height) = image.image.dimensions();
        let image = if width > max_width || height > max_height {
            image
                .image
                .resize(max_width, max_height, image::imageops::FilterType::Triangle)
        } else {
            image.image.clone()
        };
        let (width, height) = image.dimensions();
        if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
            return Err(image_api::Error::OverBudget);
        }
        let rgba = image.to_rgba8();
        Ok(image_api::Rgba {
            width,
            height,
            data: rgba.into_raw(),
        })
    }

    fn close(&mut self, image: Resource<ImageResource>) -> Result<(), image_api::Error> {
        self.table
            .delete(image)
            .map_err(|_| image_api::Error::Closed)?;
        Ok(())
    }

    fn drop(&mut self, image: Resource<ImageResource>) -> Result<(), wasmtime::Error> {
        let _ = self.table.delete(image);
        Ok(())
    }
}

fn status(status: PlaybackStatus) -> api::Status {
    api::Status {
        state: match status.state {
            PlaybackState::Idle => api::State::Idle,
            PlaybackState::Opening => api::State::Opening,
            PlaybackState::Buffering => api::State::Buffering,
            PlaybackState::Playing => api::State::Playing,
            PlaybackState::Paused => api::State::Paused,
            PlaybackState::Ended => api::State::Ended,
        },
        position_ms: status.position_ms,
        duration_ms: status.duration_ms,
        buffered_ms: status.buffered_ms,
        dropped_frames: status.dropped_frames,
    }
}

fn api_event(event: PlaybackEvent) -> api::Event {
    match event {
        PlaybackEvent::Opened(format) => api::Event::Opened(api_format(format)),
        PlaybackEvent::Buffering => api::Event::Buffering,
        PlaybackEvent::Playing => api::Event::Playing,
        PlaybackEvent::Paused => api::Event::Paused,
        PlaybackEvent::Position(position) => api::Event::Position(position),
        PlaybackEvent::FormatChanged(format) => api::Event::FormatChanged(api_format(format)),
        PlaybackEvent::Discontinuity => api::Event::Discontinuity,
        PlaybackEvent::Ended => api::Event::Ended,
    }
}

fn api_format(format: Format) -> api::Format {
    api::Format {
        container: format.container,
        video_codec: format.video_codec,
        audio_codec: format.audio_codec,
        width: format.width,
        height: format.height,
        duration_ms: format.duration_ms,
    }
}

fn encoded_chunk(chunk: EncodedChunk) -> encoded_api::Chunk {
    encoded_api::Chunk {
        data: chunk.data,
        pts_ms: chunk.pts_ms,
        duration_ms: chunk.duration_ms,
        keyframe: chunk.keyframe,
        codec_config: chunk.codec_config,
        discontinuity: chunk.discontinuity,
    }
}

fn playback_error(error: MediaError) -> api::Error {
    match error {
        MediaError::Denied => api::Error::Denied,
        MediaError::UnknownSource => api::Error::UnknownSource,
        MediaError::NotReady => api::Error::NotReady,
        MediaError::Unsupported => api::Error::Unsupported,
        MediaError::Decode => api::Error::Decode,
        MediaError::OverBudget => api::Error::OverBudget,
        MediaError::Invalid => api::Error::Invalid,
        MediaError::Closed => api::Error::Closed,
    }
}

fn encoded_error(error: MediaError) -> encoded_api::Error {
    match error {
        MediaError::Denied => encoded_api::Error::Denied,
        MediaError::UnknownSource => encoded_api::Error::UnknownSource,
        MediaError::NotReady => encoded_api::Error::NotReady,
        MediaError::Unsupported => encoded_api::Error::Unsupported,
        _ => encoded_api::Error::Invalid,
    }
}

fn image_error(error: MediaError) -> image_api::Error {
    match error {
        MediaError::Denied => image_api::Error::Denied,
        MediaError::UnknownSource => image_api::Error::UnknownSource,
        MediaError::Unsupported => image_api::Error::Unsupported,
        MediaError::OverBudget => image_api::Error::OverBudget,
        _ => image_api::Error::Invalid,
    }
}

pub fn add_to_linker<T>(linker: &mut Linker<T>) -> anyhow::Result<()>
where
    T: api::Host + encoded_api::Host + image_api::Host + 'static,
{
    api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    encoded_api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    image_api::add_to_linker::<T, HasSelf<T>>(linker, |state| state)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixtureSource {
        image: Vec<u8>,
    }

    impl MediaSource for FixtureSource {
        fn open_playback(
            &self,
            source_id: &str,
            _options: OpenOptions,
        ) -> Result<Box<dyn PlaybackSession>, MediaError> {
            if source_id != "fixture" {
                return Err(MediaError::UnknownSource);
            }
            Ok(Box::new(FixturePlayback { step: 0 }))
        }

        fn open_encoded(&self, source_id: &str) -> Result<Box<dyn EncodedSession>, MediaError> {
            if source_id != "fixture" {
                return Err(MediaError::UnknownSource);
            }
            Ok(Box::new(FixtureEncoded { sent: false }))
        }

        fn read_image(&self, source_id: &str) -> Result<Vec<u8>, MediaError> {
            if source_id != "fixture" {
                return Err(MediaError::UnknownSource);
            }
            Ok(self.image.clone())
        }
    }

    struct FixturePlayback {
        step: u8,
    }

    impl PlaybackSession for FixturePlayback {
        fn status(&mut self) -> Result<PlaybackStatus, MediaError> {
            Ok(PlaybackStatus {
                state: PlaybackState::Playing,
                position_ms: 42,
                duration_ms: Some(1_000),
                buffered_ms: Some(500),
                dropped_frames: 0,
            })
        }

        fn next(&mut self, _timeout: Duration) -> Result<Option<PlaybackEvent>, MediaError> {
            self.step += 1;
            Ok(match self.step {
                1 => Some(PlaybackEvent::Opened(Format {
                    container: Some("mp4".into()),
                    video_codec: Some("h264".into()),
                    audio_codec: Some("aac".into()),
                    width: Some(1920),
                    height: Some(1080),
                    duration_ms: Some(1_000),
                })),
                2 => Some(PlaybackEvent::Ended),
                _ => None,
            })
        }

        fn play(&mut self) -> Result<(), MediaError> {
            Ok(())
        }
        fn pause(&mut self) -> Result<(), MediaError> {
            Ok(())
        }
        fn seek(&mut self, _position_ms: u64) -> Result<(), MediaError> {
            Ok(())
        }
        fn close(&mut self) -> Result<(), MediaError> {
            Ok(())
        }
    }

    struct FixtureEncoded {
        sent: bool,
    }

    impl EncodedSession for FixtureEncoded {
        fn next(&mut self, _timeout: Duration) -> Result<Option<EncodedChunk>, MediaError> {
            if self.sent {
                return Ok(None);
            }
            self.sent = true;
            Ok(Some(EncodedChunk {
                data: vec![0, 0, 0, 1, 0x65],
                pts_ms: 0,
                duration_ms: Some(33),
                keyframe: true,
                codec_config: None,
                discontinuity: false,
            }))
        }
        fn close(&mut self) -> Result<(), MediaError> {
            Ok(())
        }
    }

    fn png() -> Vec<u8> {
        use image::{ImageBuffer, ImageFormat, Rgba};
        let image = ImageBuffer::from_pixel(2, 2, Rgba([255u8, 0, 0, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut bytes, ImageFormat::Png)
            .expect("encode fixture png");
        bytes.into_inner()
    }

    #[test]
    fn opens_playback_status_and_events() {
        let mut host = MediaHost::with_source(FixtureSource { image: png() });
        let playback = api::Host::open(
            &mut host,
            "fixture".into(),
            api::OpenOptions {
                start_ms: Some(0),
                audio: true,
                video: true,
            },
        )
        .unwrap();
        let status = api::HostPlayback::status(&mut host, playback).unwrap();
        assert_eq!(status.position_ms, 42);
        let playback = api::Host::open(
            &mut host,
            "fixture".into(),
            api::OpenOptions {
                start_ms: Some(0),
                audio: true,
                video: true,
            },
        )
        .unwrap();
        let event = api::HostPlayback::next(&mut host, playback, 100).unwrap();
        assert!(matches!(event, Some(api::Event::Opened(_))));
        let playback = api::Host::open(
            &mut host,
            "fixture".into(),
            api::OpenOptions {
                start_ms: Some(0),
                audio: true,
                video: true,
            },
        )
        .unwrap();
        api::HostPlayback::close(&mut host, playback).unwrap();
    }

    #[test]
    fn streams_encoded_chunks() {
        let mut host = MediaHost::with_source(FixtureSource { image: png() });
        let stream = encoded_api::Host::open(&mut host, "fixture".into()).unwrap();
        let chunk = encoded_api::HostEncodedStream::next(&mut host, stream, 100)
            .unwrap()
            .unwrap();
        assert!(chunk.keyframe);
        assert_eq!(chunk.data, vec![0, 0, 0, 1, 0x65]);
        let stream = encoded_api::Host::open(&mut host, "fixture".into()).unwrap();
        encoded_api::HostEncodedStream::close(&mut host, stream).unwrap();
    }

    #[test]
    fn decodes_and_bounds_images() {
        let mut host = MediaHost::with_source(FixtureSource { image: png() });
        let image = image_api::Host::open(&mut host, "fixture".into()).unwrap();
        let info = image_api::HostImage::info(&mut host, image).unwrap();
        assert_eq!(info.width, 2);
        assert_eq!(info.height, 2);
        assert_eq!(info.media_type, "image/png");
        let image = image_api::Host::open(&mut host, "fixture".into()).unwrap();
        let rgba = image_api::HostImage::rgba(&mut host, image, 1, 1).unwrap();
        assert_eq!(rgba.width, 1);
        assert_eq!(rgba.height, 1);
        assert_eq!(rgba.data.len(), 4);
        let image = image_api::Host::open(&mut host, "fixture".into()).unwrap();
        image_api::HostImage::close(&mut host, image).unwrap();
    }
}
