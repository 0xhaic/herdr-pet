//! Minimal wgpu renderer: one atlas texture, one quad, one UV uniform.

use std::sync::Arc;

use anyhow::{Context, Result};
use winit::window::Window;

use crate::pet::Atlas;
use crate::platform::OverlayBitmap;

const SHADER: &str = r#"
struct Rect {
    uv: vec4<f32>,
    // Clip-space quad: [left, top, right, bottom].
    bounds: vec4<f32>,
};

@group(0) @binding(0) var<uniform> rect: Rect;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var atlas_sampler: sampler;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    var positions = array<vec2<f32>, 6>(
        vec2<f32>(rect.bounds.x, rect.bounds.y),
        vec2<f32>(rect.bounds.z, rect.bounds.y),
        vec2<f32>(rect.bounds.x, rect.bounds.w),
        vec2<f32>(rect.bounds.x, rect.bounds.w),
        vec2<f32>(rect.bounds.z, rect.bounds.y),
        vec2<f32>(rect.bounds.z, rect.bounds.w),
    );
    var uvs = array<vec2<f32>, 6>(
        vec2<f32>(rect.uv.x, rect.uv.y),
        vec2<f32>(rect.uv.z, rect.uv.y),
        vec2<f32>(rect.uv.x, rect.uv.w),
        vec2<f32>(rect.uv.x, rect.uv.w),
        vec2<f32>(rect.uv.z, rect.uv.y),
        vec2<f32>(rect.uv.z, rect.uv.w),
    );
    var out: VertexOut;
    out.position = vec4<f32>(positions[index], 0.0, 1.0);
    out.uv = uvs[index];
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // macOS composites the non-opaque CAMetalLayer as premultiplied alpha in
    // the layer's sRGB encoding. The atlas texture already holds
    // gamma-space premultiplied values and the swapchain is non-sRGB, so
    // nothing here may decode or re-encode them: premultiplying after an
    // sRGB decode would brighten every soft edge into a white halo.
    return textureSample(atlas, atlas_sampler, in.uv);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FrameUniform {
    uv: [f32; 4],
    bounds: [f32; 4],
}

/// The uploaded chat overlay: a premultiplied BGRA bitmap covering the whole
/// window, drawn over the pet.
struct OverlayTexture {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    frame: FrameUniform,
    overlay_pipeline: wgpu::RenderPipeline,
    overlay_uniform: wgpu::Buffer,
    overlay: Option<OverlayTexture>,
}

impl Renderer {
    pub async fn new(window: Arc<Window>, atlas: &Atlas) -> Result<Self> {
        let size = window_size(&window);
        #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        // A DXGI swapchain made straight from the HWND only offers Opaque
        // composite alpha. DirectComposition adds the premultiplied-alpha path
        // the pet needs, at the cost of RenderDoc support. DX12 is forced so
        // the backend option cannot be bypassed by a Vulkan adapter.
        #[cfg(target_os = "windows")]
        {
            descriptor.backends = wgpu::Backends::DX12;
            descriptor.backend_options.dx12.presentation_system =
                wgpu::Dx12SwapchainKind::DxgiFromVisual;
        }
        let instance = wgpu::Instance::new(descriptor);
        let surface = instance
            .create_surface(window)
            .context("creating wgpu surface")?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .context("no suitable GPU adapter")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("herdr-pet device"),
                ..Default::default()
            })
            .await
            .context("creating wgpu device")?;

        let capabilities = surface.get_capabilities(&adapter);
        // Composite output is premultiplied alpha in the layer's sRGB
        // encoding (see the fragment shader). A non-sRGB swapchain passes
        // those values through untouched; an sRGB one would run them through
        // the hardware encode a second time and brighten soft edges.
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| !format.is_srgb())
            .or_else(|| capabilities.formats.first().copied())
            .context("surface reports no texture formats")?;
        // wgpu 30's Metal backend only makes CAMetalLayer non-opaque for
        // PostMultiplied; PreMultiplied leaves the layer opaque (black
        // window). The layer is still composited as premultiplied alpha,
        // which is what the shader writes. DirectComposition expects
        // PreMultiplied instead.
        #[cfg(target_os = "windows")]
        let alpha_mode = if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            wgpu::CompositeAlphaMode::Auto
        };
        #[cfg(not(target_os = "windows"))]
        let alpha_mode = if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PostMultiplied)
        {
            wgpu::CompositeAlphaMode::PostMultiplied
        } else if capabilities
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
        {
            wgpu::CompositeAlphaMode::PreMultiplied
        } else {
            wgpu::CompositeAlphaMode::Auto
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.0,
            height: size.1,
            desired_maximum_frame_latency: 2,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode,
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let texture = create_atlas_texture(&device, &queue, atlas);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("pet sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        use wgpu::util::DeviceExt;
        let frame = FrameUniform {
            uv: atlas.cell_uv(0, 0),
            // The whole window until the UI computes the pet's sub-rect.
            bounds: [-1.0, 1.0, 1.0, -1.0],
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("pet quad"),
            contents: bytemuck::bytes_of(&frame),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pet bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pet bind group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("pet shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pet pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pet pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // The quad covers the whole window; write premultiplied
                    // output as-is.
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        // The overlay quad covers the whole window. Its bitmap is already
        // premultiplied, so transparent pixels leave the pet untouched.
        let overlay_frame = FrameUniform {
            uv: [0.0, 0.0, 1.0, 1.0],
            bounds: [-1.0, 1.0, 1.0, -1.0],
        };
        let overlay_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("overlay quad"),
            contents: bytemuck::bytes_of(&overlay_frame),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let overlay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
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

        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            bind_group_layout,
            sampler,
            uniform,
            frame,
            overlay_pipeline,
            overlay_uniform,
            overlay: None,
        })
    }

    /// Swap the pet atlas without recreating the surface or pipeline.
    pub fn replace_atlas(&mut self, atlas: &Atlas) {
        let texture = create_atlas_texture(&self.device, &self.queue, atlas);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pet bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.set_uv(atlas.cell_uv(0, 0));
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub fn set_uv(&mut self, uv: [f32; 4]) {
        self.frame.uv = uv;
        self.write_frame();
    }

    /// Place the pet quad inside the window. `bounds` is clip-space
    /// `[left, top, right, bottom]`; the rest of the window stays transparent
    /// for the native glass views.
    pub fn set_frame(&mut self, uv: [f32; 4], bounds: [f32; 4]) {
        self.frame = FrameUniform { uv, bounds };
        self.write_frame();
    }

    fn write_frame(&self) {
        self.queue
            .write_buffer(&self.uniform, 0, bytemuck::bytes_of(&self.frame));
    }

    /// Upload the software-drawn chat overlay, or stop drawing it. The
    /// bitmap covers the whole window in physical pixels, so a resize
    /// recreates the texture instead of scaling the old one.
    pub fn set_overlay(&mut self, bitmap: Option<&OverlayBitmap>) {
        let Some(bitmap) = bitmap else {
            self.overlay = None;
            return;
        };
        if bitmap.width == 0 || bitmap.height == 0 {
            return;
        }
        debug_assert_eq!(
            bitmap.pixels.len(),
            (bitmap.width * bitmap.height * 4) as usize
        );
        let extent = wgpu::Extent3d {
            width: bitmap.width,
            height: bitmap.height,
            depth_or_array_layers: 1,
        };
        let copy = wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(bitmap.width * 4),
            rows_per_image: Some(bitmap.height),
        };
        if let Some(overlay) = &self.overlay
            && overlay.width == bitmap.width
            && overlay.height == bitmap.height
        {
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &overlay.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &bitmap.pixels,
                copy,
                extent,
            );
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("chat overlay"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // The bitmap stores premultiplied BGRA in sRGB encoding, like
            // the pet atlas, and goes straight to the non-sRGB swapchain.
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bitmap.pixels,
            copy,
            extent,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay bind group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.overlay_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.overlay = Some(OverlayTexture {
            texture,
            bind_group,
            width: bitmap.width,
            height: bitmap.height,
        });
    }

    pub fn render(&mut self) -> Result<()> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => return Ok(()),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("pet encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pet pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..6, 0..1);
            if let Some(overlay) = &self.overlay {
                pass.set_pipeline(&self.overlay_pipeline);
                pass.set_bind_group(0, &overlay.bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(())
    }
}

fn window_size(window: &Window) -> (u32, u32) {
    let size = window.inner_size();
    (size.width.max(1), size.height.max(1))
}

fn create_atlas_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &Atlas,
) -> wgpu::Texture {
    // Premultiply in sRGB-encoded space: that is the form the window server
    // expects from a non-opaque CAMetalLayer, and premultiplying before
    // sampling keeps linear filtering from pulling fringe colors out of
    // transparent texels.
    let mut rgba = atlas.rgba.clone();
    premultiply_encoded_alpha(&mut rgba);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pet atlas"),
        size: wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // Non-sRGB: the data is already encoded, and the shader passes it
        // straight to the non-sRGB swapchain.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(atlas.width * 4),
            rows_per_image: Some(atlas.height),
        },
        wgpu::Extent3d {
            width: atlas.width,
            height: atlas.height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// Multiply RGB by alpha in place, in the same 8-bit sRGB encoding the
/// values are stored in.
fn premultiply_encoded_alpha(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::premultiply_encoded_alpha;

    #[test]
    fn premultiplies_rgb_in_encoded_space() {
        let mut rgba = [
            200, 100, 50, 128, // 50% alpha
            255, 255, 255, 255, // opaque stays untouched
            9, 9, 9, 0, // fully transparent loses its color
        ];
        premultiply_encoded_alpha(&mut rgba);
        assert_eq!(rgba, [100, 50, 25, 128, 255, 255, 255, 255, 0, 0, 0, 0]);
    }
}
