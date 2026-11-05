//! Fixture Spore for `soil:media`.
//!
//! It exercises playback controls, encoded chunks, and bounded image readback.
//! The host source adapter owns the actual media.

wit_bindgen::generate!({ path: "wit", world: "media-probe", generate_all });

use soil::media::encoded;
use soil::media::image;
use soil::media::playback::{self, Event, OpenOptions};

struct Component;

export!(Component);

impl Guest for Component {
    fn run(_args: Vec<String>) -> Result<(), String> {
        let playback = playback::open(
            "fixture",
            OpenOptions {
                start_ms: Some(0),
                audio: true,
                video: true,
            },
        )
        .map_err(debug)?;
        playback.play().map_err(debug)?;
        playback.pause().map_err(debug)?;
        playback.seek(250).map_err(debug)?;
        let status = playback.status().map_err(debug)?;
        if status.position_ms != 42 {
            return Err(format!("unexpected position {}", status.position_ms));
        }
        match playback.next(100).map_err(debug)? {
            Some(Event::Opened(format)) => {
                if format.video_codec.as_deref() != Some("h264") {
                    return Err("expected h264 fixture".into());
                }
            }
            other => return Err(format!("expected opened event, got {other:?}")),
        }
        playback.close().map_err(debug)?;

        let stream = encoded::open("fixture").map_err(debug)?;
        let chunk = stream
            .next(100)
            .map_err(debug)?
            .ok_or("expected one encoded chunk")?;
        if !chunk.keyframe || chunk.data != [0, 0, 0, 1, 0x65] {
            return Err("unexpected encoded chunk".into());
        }
        stream.close().map_err(debug)?;

        let image = image::open("fixture").map_err(debug)?;
        let info = image.info().map_err(debug)?;
        if info.width != 2 || info.height != 2 || info.media_type != "image/png" {
            return Err("unexpected image info".into());
        }
        let rgba = image.rgba(1, 1).map_err(debug)?;
        if rgba.width != 1 || rgba.height != 1 || rgba.data.len() != 4 {
            return Err("unexpected image readback".into());
        }
        image.close().map_err(debug)?;
        Ok(())
    }
}

fn debug(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
