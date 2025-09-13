//! Request-driven background image loader.
//! Receives decode jobs (path + target size), decodes & resizes off-thread,
//! and returns RGBA8 frames without blocking the render loop.
use crossbeam_channel::{Receiver, Sender};
use std::{path::PathBuf, thread};

/// Message sent to the background loader thread.
pub enum LoaderMsg {
    /// Decode this path to the given target width/height.
    Decode(PathBuf, (u32, u32)),
    /// Stop the loader.
    Quit,
}

/// An image resized on CPU and ready for GPU upload.
pub struct PreparedImage {
    /// File name (for logging/UI).
    pub name: String,
    /// Target dimensions (width, height).
    pub size: (u32, u32),
    /// RGBA8 pixel buffer.
    pub pixels: Vec<u8>,
}

/// Spawn the request-driven loader.
pub fn spawn_loader(rx: Receiver<LoaderMsg>, tx: Sender<PreparedImage>) {
    thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            match msg {
                LoaderMsg::Quit => break,
                LoaderMsg::Decode(path, target_wh) => {
                    // Decode just this one image
                    match image::open(&path) {
                        Ok(img) => {
                            // Resize on CPU to the target so GPU upload is light
                            let resized = img.resize_exact(
                                target_wh.0,
                                target_wh.1,
                                image::imageops::Triangle,
                            );
                            let rgba = resized.to_rgba8().into_vec();
                            let _ = tx.send(PreparedImage {
                                name: path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into(),
                                size: target_wh,
                                pixels: rgba,
                            });
                        }
                        Err(_e) => {
                            // ignore broken files
                        }
                    }
                }
            }
        }
    });
}
