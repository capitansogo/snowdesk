//! Отрисовка через wgpu (на маке это Metal): один общий GPU-девайс,
//! на каждый экран своя поверхность. Рисуем инстансами — один квадрат на снежинку
//! или на столбик сугроба, форму делает фрагментный шейдер.

use std::sync::Arc;

use winit::window::Window;

use crate::sim::Instance;

const SHADER: &str = r#"
struct U { size: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var<uniform> u: U;

struct Inst {
    @location(0) pos: vec2<f32>,
    @location(1) half: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) params: vec4<f32>,
};
struct VOut {
    @builtin(position) p: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) kind: f32,
    @location(3) world: vec2<f32>,
    @location(4) params: vec4<f32>,
    @location(5) half: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, i: Inst) -> VOut {
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, 1.0));
    let c = corners[vi];
    let pt = i.pos + c * i.half;
    var o: VOut;
    o.p = vec4<f32>(pt.x / u.size.x * 2.0 - 1.0, 1.0 - pt.y / u.size.y * 2.0, 0.0, 1.0);
    o.uv = c;
    o.color = i.color;
    o.kind = i.params.x;
    o.world = pt;
    o.params = i.params;
    o.half = i.half;
    return o;
}

@fragment
fn fs(v: VOut) -> @location(0) vec4<f32> {
    var a = v.color.a;
    var rgb = v.color.rgb;
    if (v.kind < 0.5) {
        // мягкий круг
        let d = length(v.uv);
        a = a * (1.0 - smoothstep(0.4, 1.0, d));
    } else if (v.kind > 1.5) {
        let p = v.uv * v.half; // точка внутри фигуры в пунктах от центра
        if (v.kind < 2.5) {
            // скруглённый прямоугольник (SDF), радиус в params.y; радиус = половина стороны — круг
            let r = min(v.params.y, min(v.half.x, v.half.y));
            let q = abs(p) - (v.half - vec2<f32>(r, r));
            let d = length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
            a = a * clamp(0.5 - d, 0.0, 1.0);
        } else if (v.kind < 3.5) {
            // снежный шар: чёткий край + свет сверху-слева, голубоватая тень снизу
            let d = length(v.uv);
            a = a * clamp((1.0 - d) * v.half.x + 0.5, 0.0, 1.0);
            let n = vec3<f32>(v.uv.x, v.uv.y, sqrt(max(0.0, 1.0 - d * d)));
            let l = normalize(vec3<f32>(-0.45, -0.7, 0.6));
            let diff = clamp(dot(n, l), 0.0, 1.0);
            rgb = rgb * mix(vec3<f32>(0.66, 0.75, 0.9), vec3<f32>(1.0, 1.0, 1.0), 0.3 + 0.7 * diff);
        } else {
            // треугольник остриём вправо (морковка)
            let hw = (1.0 - v.uv.x) * 0.5 * v.half.y;
            let d = abs(p.y) - hw;
            a = a * clamp(0.5 - d, 0.0, 1.0);
        }
    } else {
        // столбик сугроба — трапеция: верх идёт наклонной от params.y (слева) к params.z (справа)
        let k = clamp((v.world.x - v.params.w) / 2.0, 0.0, 1.0);
        let top = mix(v.params.y, v.params.z, k);
        a = a * clamp(v.world.y - top + 0.5, 0.0, 1.0);
        // сверху белый, книзу чуть голубее (тень)
        let t = v.uv.y * 0.5 + 0.5;
        rgb = mix(rgb, rgb * vec3<f32>(0.84, 0.89, 0.97), t * t);
    }
    return vec4<f32>(rgb * a, a);
}
"#;

pub struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
    /// Переиспользуемые буферы для загрузки инстансов. `queue.write_buffer` каждый кадр
    /// заводит новый временный буфер, и Metal держит их в пуле — это +20 МБ.
    belt: std::cell::RefCell<wgpu::util::StagingBelt>,
}

pub struct Target {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    ubuf: wgpu::Buffer,
    bind: wgpu::BindGroup,
    ibuf: wgpu::Buffer,
    cap: usize,
    /// Последний кадр был пустым — повторно пустоту не рисуем.
    pub blank: bool,
}

impl Gpu {
    pub fn new(window: Arc<Window>, desc: wgpu::InstanceDescriptor) -> (Gpu, wgpu::Surface<'static>) {
        let instance = wgpu::Instance::new(desc);
        let surface = instance.create_surface(window).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .expect("adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("snow"),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .expect("device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
            .unwrap_or(caps.formats[0]);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("snow"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            ..Default::default()
        });
        let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("snow"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attrs,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let belt = std::cell::RefCell::new(wgpu::util::StagingBelt::new(device.clone(), 512 * 1024));
        (
            Gpu {
                instance,
                adapter,
                device,
                queue,
                pipeline,
                bgl,
                format,
                belt,
            },
            surface,
        )
    }

    pub fn create_surface(&self, window: Arc<Window>) -> wgpu::Surface<'static> {
        self.instance.create_surface(window).expect("surface")
    }

    /// `w_pt`, `h_pt` — размер экрана в логических пунктах (в них считает физика),
    /// `px` — размер кадра в пикселях.
    ///
    /// Если поверхность не умеет прозрачность, возвращаем ошибку: непрозрачный оверлей
    /// поверх всего экрана, сквозь который проходят клики, — худшее, что можно показать.
    pub fn target(
        &self,
        surface: wgpu::Surface<'static>,
        w_pt: f32,
        h_pt: f32,
        px: (u32, u32),
    ) -> Result<Target, String> {
        let caps = surface.get_capabilities(&self.adapter);
        let alpha = [
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
        ]
        .into_iter()
        .find(|m| caps.alpha_modes.contains(m))
        .ok_or_else(|| format!("поверхность не поддерживает прозрачность: {:?}", caps.alpha_modes))?;
        let (pw, ph) = px;
        let mut config = surface
            .get_default_config(&self.adapter, pw, ph)
            .expect("surface config");
        config.format = self.format;
        config.alpha_mode = alpha;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 1;
        config.view_formats = vec![];
        surface.configure(&self.device, &config);

        let ubuf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&ubuf, 0, bytemuck::cast_slice(&[w_pt, h_pt, 0.0, 0.0]));
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: ubuf.as_entire_binding(),
            }],
        });
        let cap = 1024;
        let ibuf = self.instance_buffer(cap);
        Ok(Target {
            surface,
            config,
            ubuf,
            bind,
            ibuf,
            cap,
            blank: false,
        })
    }

    fn instance_buffer(&self, cap: usize) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (cap * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn draw(&self, t: &mut Target, inst: &[Instance]) {
        if inst.is_empty() && t.blank {
            return;
        }
        if inst.len() > t.cap {
            t.cap = inst.len().next_power_of_two();
            t.ibuf = self.instance_buffer(t.cap);
        }
        let frame = match t.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                t.surface.configure(&self.device, &t.config);
                return;
            }
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        let mut belt = self.belt.borrow_mut();
        if !inst.is_empty() {
            let bytes: &[u8] = bytemuck::cast_slice(inst);
            let size = wgpu::BufferSize::new(bytes.len() as u64).unwrap();
            belt.write_buffer(&mut enc, &t.ibuf, 0, size)
                .copy_from_slice(bytes);
        }
        belt.finish();
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if !inst.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &t.bind, &[]);
                pass.set_vertex_buffer(0, t.ibuf.slice(..));
                pass.draw(0..4, 0..inst.len() as u32);
            }
        }
        self.queue.submit([enc.finish()]);
        belt.recall();
        let _ = self.device.poll(wgpu::PollType::Poll);
        self.queue.present(frame);
        t.blank = inst.is_empty();
        let _ = &t.ubuf;
    }
}
