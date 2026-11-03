//! Native textures must outlive every shape queued in the current egui frame.

use egui::{Context, FullOutput, TextureId};
use std::collections::{HashMap, HashSet};

pub struct TextureRetirement {
    plugin: egui::plugin::TypedPluginHandle<RetiredTextures>,
}

impl TextureRetirement {
    pub fn new(ctx: &Context) -> Self {
        Self { plugin: ctx.plugin_or_default() }
    }

    pub fn retire(&self, id: TextureId) {
        self.plugin.lock().pending.insert(id);
    }

    pub fn retire_tab(&self, id: &str, textures: &mut HashMap<String, (TextureId, u64)>, popup_textures: &mut HashMap<String, (TextureId, u64)>) {
        for textures in [textures, popup_textures] {
            if let Some((texture, _)) = textures.remove(id) {
                self.retire(texture);
            }
        }
    }
}

#[derive(Default)]
struct RetiredTextures {
    pending: HashSet<TextureId>,
}

impl egui::Plugin for RetiredTextures {
    fn debug_name(&self) -> &'static str { "native texture retirement" }

    fn output_hook(&mut self, _ctx: &Context, output: &mut FullOutput) {
        // eframe retains these deltas until paint; egui-wgpu frees after queue submission.
        output.textures_delta.free.extend(self.pending.drain());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Color32, Pos2, RawInput, Rect, Vec2};

    fn input() -> RawInput {
        RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(64.0))), ..Default::default() }
    }

    #[test]
    fn multipass_retains_main_and_popup_retirements_once_in_public_output() {
        let ctx = Context::default();
        let retirement = TextureRetirement::new(&ctx);
        let main = TextureId::User(7);
        let popup = TextureId::User(8);
        let mut passes = 0;
        let mut output = ctx.run_ui(input(), |ui| {
            passes += 1;
            retirement.retire(main);
            if passes == 1 {
                retirement.retire(popup);
                ui.ctx().request_discard("texture retirement regression");
            }
        });
        assert_eq!(passes, 2);
        assert_eq!(output.textures_delta.free, [main, popup].into_iter().collect());
        output.textures_delta.clear();
        let mut next = ctx.run_ui(input(), |_| {});
        assert!(next.textures_delta.free.is_empty());
        next.textures_delta.clear();
    }

    #[test]
    fn background_removal_is_idempotent_and_keeps_unrelated_registrations() {
        let ctx = Context::default();
        let retirement = TextureRetirement::new(&ctx);
        let second_handle = TextureRetirement::new(&ctx);
        let main = TextureId::User(10);
        let popup = TextureId::User(11);
        let keep = TextureId::User(12);
        let mut textures = HashMap::from([("background".into(), (main, 1)), ("keep".into(), (keep, 2))]);
        let mut popups = HashMap::from([("background".into(), (popup, 3))]);
        let mut output = ctx.run_ui(input(), |_| {
            retirement.retire_tab("background", &mut textures, &mut popups);
            retirement.retire_tab("background", &mut textures, &mut popups);
            second_handle.retire(main);
        });
        assert_eq!(output.textures_delta.free, [main, popup].into_iter().collect());
        assert_eq!(textures, HashMap::from([("keep".into(), (keep, 2))]));
        assert!(popups.is_empty());
        output.textures_delta.clear();
        let mut next = ctx.run_ui(input(), |_| retirement.retire_tab("background", &mut textures, &mut popups));
        assert!(next.textures_delta.free.is_empty());
        next.textures_delta.clear();
    }

    #[test]
    fn retirement_survives_an_unpainted_output_until_the_backend_consumes_it() {
        let ctx = Context::default();
        let retirement = TextureRetirement::new(&ctx);
        let id = TextureId::User(20);
        let mut pending = ctx.run_ui(input(), |_| retirement.retire(id));
        pending.append(ctx.run_ui(input(), |_| {}));
        assert_eq!(pending.textures_delta.free, [id].into_iter().collect());
        pending.textures_delta.clear();
    }

    #[cfg(target_os = "macos")]
    mod renderer {
        use super::*;
        use std::future::Future;
        use std::pin::pin;
        use std::task::{Context as TaskContext, Poll, Waker};

        struct Gpu {
            device: wgpu::Device,
            queue: wgpu::Queue,
            renderer: egui_wgpu::Renderer,
        }

        impl Gpu {
            fn new() -> Self {
                let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
                    backends: wgpu::Backends::METAL,
                    ..wgpu::InstanceDescriptor::new_without_display_handle()
                });
                let cx = &mut TaskContext::from_waker(Waker::noop());
                let Poll::Ready(Ok(adapter)) = pin!(instance.request_adapter(&Default::default())).poll(cx) else {
                    panic!("headless Metal adapter is required; no rendered proof is available");
                };
                eprintln!("texture-retirement adapter: {:?}", adapter.get_info());
                let Poll::Ready(Ok((device, queue))) = pin!(adapter.request_device(&Default::default())).poll(cx) else {
                    panic!("headless Metal device is required; no rendered proof is available");
                };
                let renderer = egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm, egui_wgpu::RendererOptions::PREDICTABLE);
                Self { device, queue, renderer }
            }

            fn register(&mut self, rgba: [u8; 4]) -> TextureId {
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("retirement source"), size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                    mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                self.queue.write_texture(texture.as_image_copy(), &rgba, wgpu::TexelCopyBufferLayout {
                    offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1),
                }, texture.size());
                let view = texture.create_view(&Default::default());
                // Only the registered bind group survives, as when a closed WebView is dropped.
                self.renderer.register_native_texture(&self.device, &view, wgpu::FilterMode::Nearest)
            }

            fn paint(&mut self, ctx: &Context, mut output: FullOutput) -> Vec<u8> {
                for (id, deltas) in output.textures_delta.set.drain() {
                    for delta in deltas { self.renderer.update_texture(&self.device, &self.queue, id, &delta); }
                }
                let primitives = ctx.tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);
                let target = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("retirement output"), size: wgpu::Extent3d { width: 64, height: 64, depth_or_array_layers: 1 },
                    mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let target_view = target.create_view(&Default::default());
                let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("retirement readback"), size: 64 * 256,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: [64, 64], pixels_per_point: output.pixels_per_point };
                let mut encoder = self.device.create_command_encoder(&Default::default());
                let user_commands = self.renderer.update_buffers(&self.device, &self.queue, &mut encoder, &primitives, &screen);
                {
                    let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("retirement render"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &target_view, resolve_target: None, depth_slice: None,
                            ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                        })],
                        ..Default::default()
                    });
                    self.renderer.render(&mut pass.forget_lifetime(), &primitives, &screen);
                }
                encoder.copy_texture_to_buffer(target.as_image_copy(), wgpu::TexelCopyBufferInfo {
                    buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(256), rows_per_image: Some(64) },
                }, target.size());
                self.queue.submit(user_commands.into_iter().chain([encoder.finish()]));
                // Match egui-wgpu's public-output contract: free only after submitting the paint.
                for id in output.textures_delta.free.drain() { self.renderer.free_texture(&id); }
                let (send, receive) = std::sync::mpsc::channel();
                readback.slice(..).map_async(wgpu::MapMode::Read, move |result| { send.send(result).unwrap(); });
                self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_secs(10)) }).unwrap();
                receive.recv_timeout(std::time::Duration::from_secs(10)).unwrap().unwrap();
                let pixels = readback.slice(..).get_mapped_range().unwrap().to_vec();
                readback.unmap();
                pixels
            }
        }

        fn removed_frame(legacy_immediate_free: bool, multipass: bool) -> ([u8; 4], [u8; 4]) {
            let ctx = Context::default();
            let retirement = TextureRetirement::new(&ctx);
            let mut gpu = Gpu::new();
            let main = gpu.register([255, 0, 0, 255]);
            let popup = gpu.register([0, 255, 0, 255]);
            let keep = gpu.register([0, 0, 255, 255]);
            let mut textures = HashMap::from([("removed".into(), (main, 1)), ("keep".into(), (keep, 2))]);
            let mut popups = HashMap::from([("removed".into(), (popup, 3))]);
            let mut passes = 0;
            let output = ctx.run_ui(input(), |ui| {
                passes += 1;
                let uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
                ui.painter().image(main, Rect::from_min_max(egui::pos2(4.0, 4.0), egui::pos2(28.0, 28.0)), uv, Color32::WHITE);
                ui.painter().image(popup, Rect::from_min_max(egui::pos2(36.0, 4.0), egui::pos2(60.0, 28.0)), uv, Color32::WHITE);
                if legacy_immediate_free {
                    // The former finish_closed_tab loop, after both shapes were queued.
                    for textures in [&mut textures, &mut popups] {
                        if let Some((id, _)) = textures.remove("removed") { gpu.renderer.free_texture(&id); }
                    }
                } else {
                    retirement.retire_tab("removed", &mut textures, &mut popups);
                    retirement.retire_tab("removed", &mut textures, &mut popups);
                    assert!(gpu.renderer.texture(&main).is_some() && gpu.renderer.texture(&popup).is_some());
                }
                if multipass && passes == 1 { ui.ctx().request_discard("retired meshes remain valid through the final pass"); }
            });
            assert_eq!(passes, if multipass { 2 } else { 1 });
            if !legacy_immediate_free {
                assert_eq!(output.textures_delta.free, [main, popup].into_iter().collect());
                assert!(gpu.renderer.texture(&main).is_some() && gpu.renderer.texture(&popup).is_some());
            }
            let pixels = gpu.paint(&ctx, output);
            assert!(gpu.renderer.texture(&main).is_none() && gpu.renderer.texture(&popup).is_none());
            assert!(gpu.renderer.texture(&keep).is_some());
            let pixel = |x: usize, y: usize| pixels[y * 256 + x * 4..y * 256 + x * 4 + 4].try_into().unwrap();
            let result = (pixel(16, 16), pixel(48, 16));
            eprintln!("texture-retirement legacy={legacy_immediate_free}: main={:?}, popup={:?}", result.0, result.1);
            retirement.retire_tab("keep", &mut textures, &mut popups);
            let next = ctx.run_ui(input(), |_| {});
            gpu.paint(&ctx, next);
            assert!(gpu.renderer.texture(&keep).is_none(), "background registrations must retire without a visible mesh");
            result
        }

        #[test]
        fn main_and_popup_render_until_submission_then_retire() {
            assert_eq!(removed_frame(false, false), ([255, 0, 0, 255], [0, 255, 0, 255]));
        }

        #[test]
        fn main_and_popup_render_after_multipass_then_retire() {
            assert_eq!(removed_frame(false, true), ([255, 0, 0, 255], [0, 255, 0, 255]));
        }

        #[test]
        fn legacy_immediate_free_reproduces_missing_main_and_popup_meshes() {
            assert_eq!(removed_frame(true, false), ([0, 0, 0, 255], [0, 0, 0, 255]));
        }
    }
}
