//! End-to-end proof: kurbo-se outlines filled by vello with the nonzero rule,
//! headless, with an animated `dash_offset` and per-frame timing.
//!
//! Renders 120 frames of a scene with inside/center/outside strokes plus an
//! animated dashed ring, re-expanding every frame with a reused
//! [`AlignedStrokeCtx`], then writes the last frame to `vello-demo.png`.
//!
//! Type identity is the point. vello re-exports the same `kurbo` that
//! kurbo-se builds on, so the `BezPath` it produces feeds `Scene::fill`
//! with no conversion.

use std::time::Instant;

use kurbo_se::{
    AlignedStrokeCtx, Cap, DashStyle, Join, StrokeAlignment, StrokeStyle, stroke_aligned,
    stroke_aligned_with,
};
use vello::kurbo::{Affine, BezPath, Circle, Rect, Shape};
use vello::peniko::{Color, Fill};
use vello::util::RenderContext;
use vello::wgpu::{self, PollType, TexelCopyBufferInfo, TexelCopyBufferLayout};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

const WIDTH: u32 = 900;
const HEIGHT: u32 = 620;
const FRAMES: usize = 120;

fn star(cx: f64, cy: f64, r_outer: f64, r_inner: f64) -> BezPath {
    let mut p = BezPath::new();
    for i in 0..10 {
        let r = if i % 2 == 0 { r_outer } else { r_inner };
        let a = std::f64::consts::PI * (i as f64) / 5.0 - std::f64::consts::FRAC_PI_2;
        let pt = (cx + r * a.cos(), cy + r * a.sin());
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    p
}

fn build_scene(scene: &mut Scene, dash_offset: f64, ctx: &mut AlignedStrokeCtx) {
    let bg = Rect::new(0.0, 0.0, WIDTH as f64, HEIGHT as f64);
    scene.fill(Fill::NonZero, Affine::IDENTITY, Color::from_rgb8(22, 24, 29), None, &bg);

    let fill_color = Color::from_rgb8(46, 52, 64);
    let stroke_color = Color::from_rgb8(110, 168, 254);

    // Three stars: inside / center / outside alignment.
    for (i, alignment) in [
        StrokeAlignment::Inside,
        StrokeAlignment::Center,
        StrokeAlignment::Outside,
    ]
    .into_iter()
    .enumerate()
    {
        let shape = star(160.0 + 290.0 * i as f64, 170.0, 110.0, 45.0);
        scene.fill(Fill::NonZero, Affine::IDENTITY, fill_color, None, &shape);
        let style = StrokeStyle::new(10.0)
            .with_alignment(alignment)
            .with_join(Join::Miter);
        let outline = stroke_aligned(&shape, &style, 0.25);
        scene.fill(Fill::NonZero, Affine::IDENTITY, stroke_color, None, &outline);
    }

    // Animated dashed ring, inside-aligned, round dash caps — the hot path:
    // re-dashed and re-expanded every frame through the reused context.
    let ring = Circle::new((450.0, 450.0), 130.0);
    scene.fill(Fill::NonZero, Affine::IDENTITY, fill_color, None, &ring.to_path(1e-3));
    let style = StrokeStyle::new(14.0)
        .with_alignment(StrokeAlignment::Inside)
        .with_dash(
            DashStyle::new(26.0, 14.0)
                .with_offset(dash_offset)
                .with_cap(Cap::Round),
        );
    let outline = stroke_aligned_with(ring, &style, 0.25, ctx);
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        Color::from_rgb8(255, 200, 87),
        None,
        outline,
    );
}

fn main() {
    let mut context = RenderContext::new();
    let device_ix = pollster::block_on(context.device(None)).expect("no compatible GPU adapter");
    let handle = &context.devices[device_ix];
    let device = &handle.device;
    let queue = &handle.queue;

    let mut renderer = Renderer::new(
        device,
        RendererOptions {
            antialiasing_support: AaSupport::area_only(),
            ..Default::default()
        },
    )
    .expect("failed to create renderer");

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("target"),
        size: wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let params = RenderParams {
        base_color: Color::from_rgb8(22, 24, 29),
        width: WIDTH,
        height: HEIGHT,
        antialiasing_method: AaConfig::Area,
    };

    let mut expand_ctx = AlignedStrokeCtx::new();
    let mut scene = Scene::new();
    let mut expand_total = 0.0f64;
    let mut frame_total = 0.0f64;

    for frame in 0..FRAMES {
        let f0 = Instant::now();
        scene.reset();
        let t0 = Instant::now();
        build_scene(&mut scene, frame as f64 * 1.5, &mut expand_ctx);
        let expand_ms = t0.elapsed().as_secs_f64() * 1e3;

        renderer
            .render_to_texture(device, queue, &scene, &view, &params)
            .expect("render failed");
        device.poll(PollType::wait_indefinitely()).expect("device poll failed");
        let frame_ms = f0.elapsed().as_secs_f64() * 1e3;
        expand_total += expand_ms;
        frame_total += frame_ms;
    }

    println!(
        "{FRAMES} frames @ {WIDTH}x{HEIGHT}: scene build (incl. all stroke expansion) avg {:.3} ms, full frame avg {:.3} ms",
        expand_total / FRAMES as f64,
        frame_total / FRAMES as f64,
    );

    // Read back the final frame and save it.
    let bytes_per_row = (WIDTH * 4).next_multiple_of(256);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: (bytes_per_row * HEIGHT) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        TexelCopyBufferInfo {
            buffer: &buffer,
            layout: TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: WIDTH,
            height: HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| tx.send(r).unwrap());
    device.poll(PollType::wait_indefinitely()).expect("device poll failed");
    rx.recv().expect("map channel closed").expect("map failed");

    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for row in 0..HEIGHT {
        let start = (row * bytes_per_row) as usize;
        pixels.extend_from_slice(&data[start..start + (WIDTH * 4) as usize]);
    }
    drop(data);

    let file = std::fs::File::create("vello-demo.png").expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), WIDTH, HEIGHT);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(&pixels).expect("png data");
    writer.finish().expect("png finish");
    println!("wrote vello-demo.png");
}
