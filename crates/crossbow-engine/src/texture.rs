//! The persistent GPU texture each web view paints into.

use std::sync::atomic::{AtomicU64, Ordering};

static GENERATION: AtomicU64 = AtomicU64::new(1);

/// A texture holding the latest frame of a web view. The view is created with
/// an sRGB format so the UI samples it in the same colour space as its own
/// images. `generation` changes whenever the texture object is recreated (the
/// UI must then re-register it); `frame` changes on every paint.
pub struct WebTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub generation: u64,
    pub frame: u64,
}

fn srgb_of(format: wgpu::TextureFormat) -> wgpu::TextureFormat {
    match format {
        wgpu::TextureFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8UnormSrgb,
        _ => wgpu::TextureFormat::Bgra8UnormSrgb,
    }
}

fn ensure<'a>(
    slot: &'a mut Option<WebTexture>,
    device: &wgpu::Device,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
) -> &'a mut WebTexture {
    let fits = slot
        .as_ref()
        .is_some_and(|t| t.width == width && t.height == height && t.texture.format() == format);
    if !fits {
        let srgb = srgb_of(format);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("crossbow web view"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // COPY_SRC: a capture reads part of the frame back.
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[srgb],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("crossbow web view (srgb)"),
            format: Some(srgb),
            ..Default::default()
        });
        *slot = Some(WebTexture {
            texture,
            view,
            width,
            height,
            generation: GENERATION.fetch_add(1, Ordering::Relaxed),
            frame: 0,
        });
    }
    slot.as_mut().unwrap()
}

/// Copy a GPU frame (imported from CEF's shared texture) into the slot.
pub(crate) fn copy_from_texture(
    slot: &mut Option<WebTexture>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    src: &wgpu::Texture,
) {
    let (w, h) = (src.width(), src.height());
    if w == 0 || h == 0 {
        return;
    }
    let format = match src.format() {
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8Unorm,
        _ => wgpu::TextureFormat::Bgra8Unorm,
    };
    let dst = ensure(slot, device, w, h, format);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("web view copy") });
    enc.copy_texture_to_texture(
        src.as_image_copy(),
        dst.texture.as_image_copy(),
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    queue.submit(Some(enc.finish()));
    dst.frame += 1;
}

/// Upload a CPU BGRA frame (software paint path) into the slot.
pub(crate) fn upload_bgra(
    slot: &mut Option<WebTexture>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bgra: &[u8],
    width: u32,
    height: u32,
) {
    if width == 0 || height == 0 || bgra.len() < (width * height * 4) as usize {
        return;
    }
    let dst = ensure(slot, device, width, height, wgpu::TextureFormat::Bgra8Unorm);
    queue.write_texture(
        dst.texture.as_image_copy(),
        bgra,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * width), rows_per_image: Some(height) },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
    dst.frame += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// A capture reads a part of the frame back, so both paint paths make
    /// textures that allow copies from them.
    #[test]
    fn every_web_view_texture_can_be_read_back_for_a_capture() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::METAL, ..wgpu::InstanceDescriptor::new_without_display_handle() });
        let cx = &mut Context::from_waker(Waker::noop());
        let Poll::Ready(Ok(adapter)) = pin!(instance.request_adapter(&Default::default())).poll(cx) else { panic!("a headless Metal adapter is required") };
        let Poll::Ready(Ok((device, queue))) = pin!(adapter.request_device(&Default::default())).poll(cx) else { panic!("a headless Metal device is required") };
        let mut software = None;
        upload_bgra(&mut software, &device, &queue, &[0; 16], 2, 2);
        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut accelerated = None;
        copy_from_texture(&mut accelerated, &device, &queue, &source);
        for slot in [software, accelerated] {
            let texture = slot.expect("a frame").texture;
            assert!(texture.usage().contains(wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST));
        }
    }
}
