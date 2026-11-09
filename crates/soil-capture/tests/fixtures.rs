use std::time::Duration;

use soil_capture::camera::{CameraSource, PixelFormat};
use soil_capture::fixture::{FixtureCameraSource, FixtureMicSource};
use soil_capture::mic::MicSource;
use soil_capture::{CaptureError, MAX_CAMERA_BYTES};

#[test]
fn fixture_microphone_returns_typed_chunks_and_then_none() {
    let source = FixtureMicSource::tone(16_000, 320, 0.25);
    let mut session = source.open("fixture test").unwrap();
    assert_eq!(session.format().sample_rate, 16_000);
    assert_eq!(session.format().channels, 1);

    let chunk = session
        .read(320, Duration::ZERO)
        .unwrap()
        .expect("first chunk");
    assert_eq!(chunk.frames, 320);
    assert_eq!(chunk.samples.len(), 320);
    assert!(chunk.samples.iter().any(|sample| sample.abs() > 0.0));
    assert!(session.read(320, Duration::ZERO).unwrap().is_none());
}

#[test]
fn fixture_microphone_rejects_oversized_reads() {
    let source = FixtureMicSource::tone(16_000, 320, 0.25);
    let mut session = source.open("fixture test").unwrap();
    assert_eq!(
        session.read(soil_capture::MAX_MIC_FRAMES + 1, Duration::ZERO),
        Err(CaptureError::Invalid)
    );
}

#[test]
fn fixture_camera_returns_bounded_rgba_frames() {
    let source = FixtureCameraSource::solid(4, 4, [10, 20, 30, 255], 2);
    let mut session = source.open("fixture test").unwrap();
    let format = session.format();
    assert_eq!(format.width, 4);
    assert_eq!(format.height, 4);
    assert_eq!(format.pixel_format, PixelFormat::Rgba8);

    let first = session
        .next_frame(Duration::ZERO)
        .unwrap()
        .expect("first frame");
    assert_eq!(first.sequence, 1);
    assert_eq!(first.data.len(), 64);
    assert!(first.data.len() <= MAX_CAMERA_BYTES);
    assert_eq!(&first.data[..4], &[10, 20, 30, 255]);
    assert!(
        session
            .next_frame(Duration::ZERO)
            .unwrap()
            .expect("second frame")
            .sequence
            == 2
    );
    assert!(session.next_frame(Duration::ZERO).unwrap().is_none());
}
