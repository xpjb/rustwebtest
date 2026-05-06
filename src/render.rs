// winit + wgpu sprite renderer.
//
// Architecture:
//   * winit owns the event loop. On web the loop is driven by the browser's
//     requestAnimationFrame; on native it runs in poll mode.
//   * Vertex data is rebuilt on the main thread by merging the latest
//     Snapshot from each worker (try_recv only — never blocking).
//   * Overlap detection runs on main and emits plings via the audio module.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use bytemuck::cast_slice;
use wgpu::util::DeviceExt;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::atlas;
use crate::sim::{Snapshot, Vertex};
use crate::workers::{self, WorkerPool};

type BoxErr = Box<dyn std::error::Error>;

const SHADER: &str = r#"
struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv:  vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var o: VsOut;
    o.clip = vec4<f32>(in.pos, 0.0, 1.0);
    o.uv = in.uv;
    return o;
}

@group(0) @binding(0) var atlas_tex: texture_2d<f32>;
@group(0) @binding(1) var atlas_smp: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(atlas_tex, atlas_smp, in.uv);
    if (c.a < 0.01) { discard; }
    return c;
}
"#;

struct Gfx {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    vbuf: wgpu::Buffer,
    ibuf: wgpu::Buffer,
    vbuf_capacity: u64,
    ibuf_capacity: u64,
    index_count: u32,
}

impl Gfx {
    async fn new(window: Arc<Window>) -> Result<Self, BoxErr> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance.create_surface(window.clone())?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or("no compatible wgpu adapter")?;

        let required_limits = if cfg!(target_arch = "wasm32") {
            wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits())
        } else {
            wgpu::Limits::default()
        };

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("device"),
                    required_features: wgpu::Features::empty(),
                    required_limits,
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
            .await?;

        // On web, `inner_size` starts at 0×0 until winit's ResizeObserver runs; that
        // happens after the window is registered but typically *after* the first
        // poll point in this async fn. Reading size here (after awaits) matches the
        // canvas backing store instead of clamping to 1×1 until a manual resize.
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: caps.present_modes[0],
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // -- Atlas texture upload -----------------------------------------
        // The atlas PNG is `include_bytes!`-bundled at compile time. Decode it
        // here so we can upload raw RGBA into a wgpu texture.
        let mut decoder = png::Decoder::new(std::io::Cursor::new(atlas::ATLAS_PNG));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info()?;
        let mut rgba = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
        let info = reader.next_frame(&mut rgba)?;
        let aw = info.width;
        let ah = info.height;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d {
                width: aw,
                height: ah,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * aw),
                rows_per_image: Some(ah),
            },
            wgpu::Extent3d {
                width: aw,
                height: ah,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas-bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprite"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("pl"),
                bind_group_layouts: &[&bgl],
                push_constant_ranges: &[],
            });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 1,
                },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sprite-pipe"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let initial_vbuf_cap = (4 * 256 * std::mem::size_of::<Vertex>()) as u64;
        let initial_ibuf_cap = (6 * 256 * std::mem::size_of::<u16>()) as u64;
        let vbuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vbuf"),
            size: initial_vbuf_cap,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let ibuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("ibuf"),
            contents: cast_slice(&build_indices(256)),
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        });

        Ok(Gfx {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            vbuf,
            ibuf,
            vbuf_capacity: initial_vbuf_cap,
            ibuf_capacity: initial_ibuf_cap,
            index_count: 0,
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
    }

    fn upload(&mut self, verts: &[Vertex]) {
        if verts.is_empty() {
            self.index_count = 0;
            return;
        }
        let quad_count = verts.len() / 4;
        let needed_v = (verts.len() * std::mem::size_of::<Vertex>()) as u64;
        if needed_v > self.vbuf_capacity {
            let new_cap = (needed_v * 2).next_power_of_two();
            self.vbuf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vbuf"),
                size: new_cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.vbuf_capacity = new_cap;
        }
        let needed_i = (quad_count * 6 * std::mem::size_of::<u16>()) as u64;
        if needed_i > self.ibuf_capacity {
            let new_quads = (quad_count as u64 * 2).next_power_of_two() as usize;
            self.ibuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ibuf"),
                contents: cast_slice(&build_indices(new_quads)),
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            });
            self.ibuf_capacity = (new_quads * 6 * std::mem::size_of::<u16>()) as u64;
        }
        self.queue.write_buffer(&self.vbuf, 0, cast_slice(verts));
        self.index_count = (quad_count * 6) as u32;
    }

    fn render(&mut self) -> Result<(), wgpu::SurfaceError> {
        let frame = self.surface.get_current_texture()?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("enc") });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.07,
                            b: 0.10,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            if self.index_count > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vbuf.slice(..));
                pass.set_index_buffer(self.ibuf.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..self.index_count, 0, 0..1);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

fn build_indices(quad_count: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(quad_count * 6);
    for q in 0..quad_count {
        let b = (q * 4) as u16;
        out.extend_from_slice(&[b, b + 1, b + 2, b, b + 2, b + 3]);
    }
    out
}

// =============================================================================
// winit ApplicationHandler
// =============================================================================

struct App {
    window: Option<Arc<Window>>,
    gfx: Rc<RefCell<Option<Gfx>>>,
    pool: Option<WorkerPool>,
    snapshots: Vec<Option<Snapshot>>,
    overlaps: HashSet<(u32, u32)>,
    merged_verts: Vec<Vertex>,
    bgm: &'static [u8],
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gfx: Rc::new(RefCell::new(None)),
            pool: None,
            snapshots: Vec::new(),
            overlaps: HashSet::new(),
            merged_verts: Vec::new(),
            bgm: crate::BGM_OGG,
        }
    }

    fn merge_snapshots(&mut self) {
        self.merged_verts.clear();
        for snap in self.snapshots.iter().flatten() {
            self.merged_verts.extend_from_slice(&snap.verts);
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        #[allow(unused_mut)]
        let mut attrs = Window::default_attributes().with_title("rustwebtest");

        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast;
            use winit::platform::web::WindowAttributesExtWebSys;
            let canvas = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| {
                    d.get_element_by_id("canvas").or_else(|| {
                        let c = d.create_element("canvas").ok()?;
                        let _ = c.set_id("canvas");
                        d.body()?.append_child(&c).ok()?;
                        Some(c)
                    })
                })
                .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok());
            attrs = attrs.with_canvas(canvas);
        }

        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("create_window: {e}");
                return;
            }
        };
        self.window = Some(window.clone());

        // Build Gfx async on wasm; sync on native.
        #[cfg(target_arch = "wasm32")]
        {
            let gfx_slot = self.gfx.clone();
            let win = window.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match Gfx::new(win.clone()).await {
                    Ok(g) => {
                        *gfx_slot.borrow_mut() = Some(g);
                        win.request_redraw();
                    }
                    Err(e) => log::error!("Gfx::new: {e}"),
                }
            });
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let g = pollster::block_on(Gfx::new(window.clone())).expect("Gfx::new");
            *self.gfx.borrow_mut() = Some(g);
        }

        self.pool = Some(workers::spawn_pool(
            atlas::ATLAS_W as f32,
            atlas::ATLAS_H as f32,
        ));
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(g) = self.gfx.borrow_mut().as_mut() {
                    g.resize(size.width, size.height);
                }
            }
            WindowEvent::MouseInput { .. } | WindowEvent::Touch(_) => {
                crate::audio::try_init(self.bgm);
            }
            WindowEvent::RedrawRequested => {
                if let (Some(w), Some(g)) = (self.window.as_ref(), self.gfx.borrow_mut().as_mut())
                {
                    let s = w.inner_size();
                    if s.width > 0
                        && s.height > 0
                        && (g.config.width != s.width || g.config.height != s.height)
                    {
                        g.resize(s.width, s.height);
                    }
                }
                if let Some(pool) = &self.pool {
                    workers::poll_latest(pool, &mut self.snapshots);
                }
                let new_overlaps =
                    workers::detect_new_overlaps(&self.snapshots, &mut self.overlaps);
                for (a, b) in new_overlaps {
                    let f = 500.0
                        + (((a.wrapping_mul(2654435761)) ^ b) as f32 % 600.0).abs();
                    crate::audio::pling(f);
                }

                self.merge_snapshots();
                if let Some(g) = self.gfx.borrow_mut().as_mut() {
                    g.upload(&self.merged_verts);
                    if let Err(e) = g.render() {
                        match e {
                            wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated => {
                                let (w, h) = (g.config.width, g.config.height);
                                g.resize(w, h);
                            }
                            _ => log::warn!("render: {e:?}"),
                        }
                    }
                }
                if let Some(w) = self.window.as_ref() {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

pub async fn run() -> Result<(), BoxErr> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let app = App::new();

    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        // spawn_app() returns immediately and registers the loop on the
        // browser's animation frame queue.
        event_loop.spawn_app(app);
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut app = app;
        event_loop.run_app(&mut app)?;
        Ok(())
    }
}
