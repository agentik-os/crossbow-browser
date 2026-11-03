use super::*;
use std::future::Future;
use std::pin::pin;
use std::task::{Context as TaskContext, Poll, Waker};

/// A headless Metal device, as the texture retirement tests use.
fn gpu() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends: wgpu::Backends::METAL, ..wgpu::InstanceDescriptor::new_without_display_handle() });
    let cx = &mut TaskContext::from_waker(Waker::noop());
    let Poll::Ready(Ok(adapter)) = pin!(instance.request_adapter(&Default::default())).poll(cx) else { panic!("a headless Metal adapter is required") };
    let Poll::Ready(Ok(device)) = pin!(adapter.request_device(&Default::default())).poll(cx) else { panic!("a headless Metal device is required") };
    device
}

/// A page frame as CEF paints it: BGRA, premultiplied, `width` by `height`,
/// white but for `boxes` of (x, y, width, height, [b, g, r, a]) in pixels.
fn frame(device: &wgpu::Device, queue: &wgpu::Queue, width: u32, height: u32, boxes: &[(u32, u32, u32, u32, [u8; 4])]) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("capture test frame"),
        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Bgra8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let mut bytes = vec![255u8; (width * height * 4) as usize];
    for &(x, y, w, h, color) in boxes {
        for row in y..y + h {
            for column in x..x + w {
                let at = ((row * width + column) * 4) as usize;
                bytes[at..at + 4].copy_from_slice(&color);
            }
        }
    }
    queue.write_texture(texture.as_image_copy(), &bytes, wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(width * 4), rows_per_image: Some(height) }, texture.size());
    texture
}

const RED_BGRA: [u8; 4] = [0, 0, 255, 255];

#[test]
fn a_selection_reads_the_pixels_shown_under_it_at_any_scale_and_stops_at_the_frame() {
    let page = Rect::from_min_size(pos2(200.0, 40.0), vec2(800.0, 600.0));
    for ppp in [1.0f32, 2.0] {
        let texture = ((800.0 * ppp) as u32, (600.0 * ppp) as u32);
        let k = ppp as u32;
        let inside = Rect::from_min_max(pos2(300.0, 140.0), pos2(400.0, 190.0));
        assert_eq!(pixels(inside, page, texture, ppp), Some(PixelRect { x: 100 * k, y: 100 * k, width: 100 * k, height: 50 * k }), "at {ppp}x");
        // Past the page's bottom-right corner, and over the sidebar to its left.
        let past = Rect::from_min_max(pos2(900.0, 600.0), pos2(1300.0, 900.0));
        assert_eq!(pixels(past, page, texture, ppp), Some(PixelRect { x: 700 * k, y: 560 * k, width: 100 * k, height: 40 * k }));
        let over_sidebar = Rect::from_min_max(pos2(0.0, 40.0), pos2(250.0, 80.0));
        assert_eq!(pixels(over_sidebar, page, texture, ppp), Some(PixelRect { x: 0, y: 0, width: 50 * k, height: 40 * k }));
        assert_eq!(pixels(Rect::from_min_max(pos2(0.0, 0.0), pos2(150.0, 30.0)), page, texture, ppp), None, "nothing of the page");
        // While the window grows, the frame is still the old size: only what it shows counts.
        let old = ((700.0 * ppp) as u32, (500.0 * ppp) as u32);
        assert_eq!(pixels(page, page, old, ppp), Some(PixelRect { x: 0, y: 0, width: old.0, height: old.1 }));
    }
    assert_eq!(pixels(Rect::from_min_size(pos2(300.0, 300.0), vec2(0.2, 0.2)), page, (800, 600), 1.0), None, "less than a pixel");
    // A drag is kept within the page, and a moved selection stays whole inside it.
    assert_eq!(dragged(pos2(1100.0, 700.0), pos2(500.0, 10.0), page), Rect::from_min_max(pos2(500.0, 40.0), pos2(1000.0, 640.0)));
    let selection = Rect::from_min_size(pos2(900.0, 500.0), vec2(80.0, 60.0));
    assert_eq!(moved(selection, vec2(500.0, 500.0), page), Rect::from_min_size(pos2(920.0, 580.0), vec2(80.0, 60.0)));
    assert_eq!(moved(selection, vec2(-2000.0, 0.0), page), Rect::from_min_size(pos2(200.0, 500.0), vec2(80.0, 60.0)));
}

/// Page zoom scales what CEF paints, so a box drawn at any zoom and pixel
/// ratio is captured exactly, from its on-screen outline.
#[test]
fn a_box_on_the_page_is_captured_exactly_at_zoom_50_100_and_200_percent_and_1x_and_2x() {
    let (device, queue) = gpu();
    let page = Rect::from_min_size(pos2(240.0, 52.0), vec2(400.0, 300.0));
    for zoom in [0.5f32, 1.0, 2.0] {
        for ppp in [1.0f32, 2.0] {
            // A 100 × 50 CSS-pixel box at CSS (20, 10).
            let scale = zoom * ppp;
            let (x, y, w, h) = ((20.0 * scale) as u32, (10.0 * scale) as u32, (100.0 * scale) as u32, (50.0 * scale) as u32);
            let size = ((400.0 * ppp) as u32, (300.0 * ppp) as u32);
            let texture = frame(&device, &queue, size.0, size.1, &[(x, y, w, h, RED_BGRA)]);
            let outline = Rect::from_min_size(page.min + vec2(20.0, 10.0) * zoom, vec2(100.0, 50.0) * zoom);
            let region = pixels(outline, page, size, ppp).expect("the box is on the page");
            let image = read(&device, &queue, &texture, region).unwrap();
            assert_eq!((image.width, image.height), (w, h), "zoom {zoom}, {ppp}x");
            assert!(image.rgba.chunks_exact(4).all(|pixel| pixel == [255, 0, 0, 255]), "only the box, in RGBA, at zoom {zoom}, {ppp}x");
        }
    }
}

#[test]
fn the_capture_is_straight_alpha_rgba_and_never_reads_past_the_frame() {
    let (device, queue) = gpu();
    // Premultiplied half-transparent red, then opaque blue.
    let texture = frame(&device, &queue, 70, 3, &[(0, 0, 1, 1, [0, 0, 128, 128]), (1, 0, 1, 1, [255, 0, 0, 255]), (2, 0, 1, 1, [0, 0, 0, 0])]);
    let image = read(&device, &queue, &texture, PixelRect { x: 0, y: 0, width: 70, height: 3 }).unwrap();
    assert_eq!((image.width, image.height, image.rgba.len()), (70, 3, 70 * 3 * 4), "rows unpadded");
    assert_eq!(&image.rgba[..12], &[255, 0, 0, 128, 0, 0, 255, 255, 0, 0, 0, 0]);
    for outside in [PixelRect { x: 60, y: 0, width: 11, height: 1 }, PixelRect { x: 0, y: 2, width: 1, height: 2 }, PixelRect { x: 0, y: 0, width: 0, height: 1 }, PixelRect { x: u32::MAX, y: 0, width: 2, height: 1 }] {
        assert!(read(&device, &queue, &texture, outside).is_err(), "{outside:?}");
    }
}

#[test]
fn a_capture_is_a_png_of_exactly_those_pixels_marked_with_their_density() {
    let image = Image { width: 3, height: 2, rgba: (0..24).map(|i| (i * 10) as u8).collect() };
    for (ppp, per_metre) in [(1.0f32, 2835u32), (2.0, 5669)] {
        let bytes = png(&image, ppp).unwrap();
        let decoded = image::load_from_memory(&bytes).expect("a PNG whose chunks check out").to_rgba8();
        assert_eq!((decoded.width(), decoded.height(), decoded.as_raw().as_slice()), (3, 2, image.rgba.as_slice()));
        // Chunks in order: IHDR, then pHYs.
        let mut at = 8;
        let mut chunks = Vec::new();
        while at + 8 <= bytes.len() {
            let length = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let kind = String::from_utf8_lossy(&bytes[at + 4..at + 8]).to_string();
            let body = &bytes[at + 8..at + 8 + length];
            let crc = u32::from_be_bytes(bytes[at + 8 + length..at + 12 + length].try_into().unwrap());
            assert_eq!(crc, crc32(&bytes[at + 4..at + 8 + length]), "{kind} checks out");
            if kind == "pHYs" {
                assert_eq!(body, [per_metre.to_be_bytes().as_slice(), per_metre.to_be_bytes().as_slice(), &[1]].concat(), "{ppp}x");
            }
            chunks.push(kind);
            at += 12 + length;
        }
        assert_eq!(&chunks[..2], ["IHDR", "pHYs"]);
        assert_eq!(chunks.last().map(String::as_str), Some("IEND"));
    }
    assert_eq!(crc32(b"IEND"), 0xae42_6082, "the CRC every PNG's end carries");
}
