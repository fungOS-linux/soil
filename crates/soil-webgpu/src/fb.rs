//! `/dev/fb0` presenter for the S22 outer panel.
//!
//! The outer display is a plain 16-bpp framebuffer (`panel-mipi-dbi-spi`,
//! 128x128, stride 256 bytes). A window system or a DRM modeset would be
//! machinery with nothing to drive: the guest has already rendered the frame,
//! so it is converted to RGB565 and written into the framebuffer.
//!
//! This is a device presenter, not a compositor. It owns exactly one panel and
//! presents one frame at a time; a host that needs z-order or hit testing uses
//! `canvas-winit` instead.

use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::{CanvasFrame, CanvasGpuPresenter};

pub struct FramebufferPresenter {
    file: File,
    path: PathBuf,
    width: u32,
    height: u32,
    stride: usize,
}

impl FramebufferPresenter {
    /// Open a framebuffer device. `width`/`height` are the logical surface the
    /// Spore lays out against, which is the panel extent in device pixels.
    ///
    /// Geometry comes from sysfs rather than a flag: the driver is the only
    /// authority on the stride, and a wrong stride writes skewed rows instead
    /// of failing.
    pub fn open(path: impl AsRef<Path>, width: u32, height: u32) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let sysfs = Path::new("/sys/class/graphics")
            .join(path.file_name().unwrap_or_else(|| "fb0".as_ref()));
        let bits = read_u32(&sysfs.join("bits_per_pixel")).unwrap_or(16);
        if bits != 16 {
            bail!(
                "{}: {bits}-bpp framebuffer is unsupported (this presenter writes RGB565)",
                path.display()
            );
        }
        let stride = read_u32(&sysfs.join("stride"))
            .map(|value| value as usize)
            .unwrap_or(width as usize * 2);
        if stride < width as usize * 2 {
            bail!(
                "{}: stride {stride} is narrower than {width} RGB565 pixels",
                path.display()
            );
        }
        Ok(Self {
            file,
            path,
            width,
            height,
            stride,
        })
    }
}

fn read_u32(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

impl CanvasGpuPresenter for FramebufferPresenter {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn present(&mut self, frame: CanvasFrame) -> Result<()> {
        anyhow::ensure!(
            frame.rgba.len() == (frame.width * frame.height * 4) as usize,
            "frame is not tightly packed RGBA8"
        );
        let mut buffer = vec![0u8; self.stride * self.height as usize];
        for y in 0..self.height {
            // Nearest neighbour, so a frame rendered at another extent still
            // fills the panel instead of leaving the rest of it stale.
            let source_y = (y as u64 * frame.height as u64 / self.height as u64) as u32;
            let row = y as usize * self.stride;
            for x in 0..self.width {
                let source_x = (x as u64 * frame.width as u64 / self.width as u64) as u32;
                let at = ((source_y * frame.width + source_x) * 4) as usize;
                let (r, g, b) = (
                    frame.rgba[at],
                    frame.rgba[at + 1],
                    frame.rgba[at + 2],
                );
                let pixel = ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3);
                let at = row + x as usize * 2;
                buffer[at] = (pixel & 0xff) as u8;
                buffer[at + 1] = (pixel >> 8) as u8;
            }
        }
        self.file
            .seek(SeekFrom::Start(0))
            .with_context(|| format!("seek {}", self.path.display()))?;
        self.file
            .write_all(&buffer)
            .with_context(|| format!("write {}", self.path.display()))?;
        self.file.flush().ok();
        Ok(())
    }
}
