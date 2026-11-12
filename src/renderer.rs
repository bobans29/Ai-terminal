//! GPU text renderer.
//!
//! A monospace grid is a list of colored rectangles plus a glyph atlas. wgpu
//! draws both. The atlas is rasterized on the CPU with fontdue (one codepoint
//! per cell, which matches how the terminal grid stores characters) and
//! sampled on the GPU. Tabs share the device and the atlas.

use std::collections::HashMap;
use std::sync::Arc;

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::point_to_viewport;
use alacritty_terminal::vte::ansi::CursorShape;
use bytemuck::{Pod, Zeroable};
use winit::window::Window;

use crate::color::{self, Palette, Rgb};
use crate::font::FontSet;
use crate::tab_bar;
use crate::terminal::Terminal;

const ATLAS_START: u32 = 1024;
const ATLAS_MAX: u32 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorPaint {
    Off,
    Active,
    Hollow,
}

pub enum DrawResult {
    Ok,
    /// The swapchain was outdated; the caller should ask for another frame.
    Retry,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
    mode: u32,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    atlas_texture: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    atlas: Atlas,
    fonts: FontSet,
    vertex_buf: wgpu::Buffer,
    vertex_capacity: usize,
    family: String,
    font_size: f32,
}

impl Renderer {
    pub fn new(
        window: Arc<Window>,
        fonts: FontSet,
        family: String,
        font_size: f32,
        background: Rgb,
    ) -> Result<Self, String> {
        let size = window.inner_size();
        let gpu = init_gpu(window, size.width.max(1), size.height.max(1))?;
        let atlas = Atlas::new(ATLAS_START);
        let (atlas_texture, atlas_view) = make_texture(&gpu.device, atlas.width, atlas.height);
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("atlas"),
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
        let bind_group = make_bind_group(&gpu.device, &bind_group_layout, &atlas_view, &sampler);
        let pipeline = make_pipeline(&gpu.device, &bind_group_layout, gpu.config.format);
        let vertex_capacity = 4096;
        let vertex_buf = vertex_buffer(&gpu.device, vertex_capacity);
        let _ = background;
        Ok(Self {
            surface: gpu.surface,
            device: gpu.device,
            queue: gpu.queue,
            config: gpu.config,
            pipeline,
            bind_group_layout,
            bind_group,
            sampler,
            atlas_texture,
            atlas_view,
            atlas,
            fonts,
            vertex_buf,
            vertex_capacity,
            family,
            font_size,
        })
    }

    pub fn cell_size(&self) -> (f32, f32) {
        (self.fonts.cell_width, self.fonts.cell_height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if self.config.width == width && self.config.height == height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Reload the face when the window scale factor changes. Returns whether
    /// the cell size changed.
    pub fn set_scale(&mut self, scale: f64) -> Result<bool, String> {
        let px = (self.font_size * scale as f32).clamp(6.0, 128.0);
        if (px - self.fonts.px).abs() < 0.25 {
            return Ok(false);
        }
        self.fonts = FontSet::load(&self.family, px)?;
        self.atlas.reset();
        Ok(true)
    }

    pub fn draw(
        &mut self,
        terminal: &Terminal,
        palette: &Palette,
        cursor: CursorPaint,
        titles: &[String],
        active: usize,
    ) -> DrawResult {
        if self.config.width == 0 || self.config.height == 0 {
            return DrawResult::Ok;
        }
        let mut verts = self.build_vertices(terminal, palette, cursor, titles, active);
        // Rasterizing a new glyph can grow the atlas. UVs baked above would
        // then be wrong, so build the frame once more against the final atlas.
        if self.atlas.grew {
            self.atlas.grew = false;
            verts = self.build_vertices(terminal, palette, cursor, titles, active);
            self.atlas.grew = false;
        }
        self.sync_atlas();
        self.ensure_vertex_buffer(verts.len());
        if !verts.is_empty() {
            self.queue
                .write_buffer(&self.vertex_buf, 0, bytemuck::cast_slice(&verts));
        }

        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return DrawResult::Retry;
            }
            Err(wgpu::SurfaceError::Timeout) => return DrawResult::Ok,
            Err(wgpu::SurfaceError::OutOfMemory) => {
                eprintln!("aiterm: gpu out of memory");
                return DrawResult::Ok;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cells"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(gpu_color(palette.background)),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if !verts.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buf.slice(..));
                pass.draw(0..verts.len() as u32, 0..1);
            }
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        DrawResult::Ok
    }

    fn build_vertices(
        &mut self,
        terminal: &Terminal,
        palette: &Palette,
        cursor: CursorPaint,
        titles: &[String],
        active: usize,
    ) -> Vec<Vertex> {
        let mut verts = Vec::new();
        let width = self.config.width as f32;
        let height = self.config.height as f32;
        let cell_w = self.fonts.cell_width;
        let cell_h = self.fonts.cell_height;
        let ascent = self.fonts.ascent;
        let content = terminal.content();
        let colors = content.colors;
        let cursor_point = content.cursor.point;
        let cursor_shape = content.cursor.shape;
        let selection = content.selection;
        let display_offset = content.display_offset;
        // Backgrounds first, then glyphs. A wide character is drawn from its
        // first cell and overlaps the spacer; the spacer's background must not
        // paint over that glyph.
        let mut backs = Vec::new();
        let mut front = Vec::new();

        for indexed in content.display_iter {
            let Some(vp) = point_to_viewport(display_offset, indexed.point) else {
                continue;
            };
            let x = vp.column.0 as f32 * cell_w;
            let y = cell_h + vp.line as f32 * cell_h;
            let flags = indexed.cell.flags;
            let mut fg = color::resolve(palette, colors, indexed.cell.fg);
            let mut bg = color::resolve(palette, colors, indexed.cell.bg);
            if flags.contains(Flags::DIM) {
                fg = fg.dim();
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let selected = selection
                .as_ref()
                .is_some_and(|range| range.contains_cell(&indexed, cursor_point, cursor_shape));
            if selected {
                bg = palette.selection;
            }

            let on_wide_spacer = flags.contains(Flags::WIDE_CHAR_SPACER)
                && indexed.point.line == cursor_point.line
                && indexed.point.column.0 == cursor_point.column.0.saturating_add(1);
            let on_cursor = indexed.point == cursor_point || on_wide_spacer;
            let block =
                on_cursor && cursor == CursorPaint::Active && cursor_shape == CursorShape::Block;
            if block {
                // Glyph keeps the previous background so it stays visible on the block.
                if indexed.point == cursor_point {
                    fg = bg;
                }
                bg = palette.cursor;
            }

            if bg != palette.background || block || selected {
                push_solid(&mut backs, width, height, x, y, cell_w, cell_h, bg);
            }

            let spacer =
                flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
            let ch = indexed.cell.c;
            let drawable = !spacer
                && !flags.contains(Flags::HIDDEN)
                && ch != ' '
                && ch != '\t'
                && !ch.is_control();
            if drawable {
                let style = style_index(flags);
                let glyph = self.atlas.glyph(&self.fonts, ch, style);
                if glyph.w > 0 {
                    let (u0, v0, u1, v1) = self.atlas.uv(glyph);
                    let gx = x + glyph.xmin as f32;
                    let gy = y + ascent - (glyph.ymin as f32 + glyph.h as f32);
                    push_glyph(
                        &mut front,
                        width,
                        height,
                        gx,
                        gy,
                        glyph.w as f32,
                        glyph.h as f32,
                        u0,
                        v0,
                        u1,
                        v1,
                        fg,
                    );
                    if glyph.fake_bold {
                        push_glyph(
                            &mut front,
                            width,
                            height,
                            gx + 1.0,
                            gy,
                            glyph.w as f32,
                            glyph.h as f32,
                            u0,
                            v0,
                            u1,
                            v1,
                            fg,
                        );
                    }
                }
            }

            let span = if flags.contains(Flags::WIDE_CHAR) {
                cell_w * 2.0
            } else {
                cell_w
            };
            if flags.intersects(Flags::ALL_UNDERLINES) && !flags.contains(Flags::HIDDEN) {
                let uy = y + ascent + 1.0;
                push_solid(&mut front, width, height, x, uy, span, 1.0, fg);
            }
            if flags.contains(Flags::STRIKEOUT) && !flags.contains(Flags::HIDDEN) {
                let sy = y + cell_h * 0.55;
                push_solid(&mut front, width, height, x, sy, span, 1.0, fg);
            }

            if indexed.point == cursor_point && cursor_shape != CursorShape::Hidden {
                match cursor {
                    CursorPaint::Hollow => {
                        stroke_rect(
                            &mut front,
                            width,
                            height,
                            x,
                            y,
                            span,
                            cell_h,
                            palette.cursor,
                        );
                    }
                    CursorPaint::Active => match cursor_shape {
                        CursorShape::Beam => {
                            push_solid(
                                &mut front,
                                width,
                                height,
                                x,
                                y,
                                2.0,
                                cell_h,
                                palette.cursor,
                            );
                        }
                        CursorShape::Underline => {
                            push_solid(
                                &mut front,
                                width,
                                height,
                                x,
                                y + cell_h - 2.0,
                                span,
                                2.0,
                                palette.cursor,
                            );
                        }
                        CursorShape::HollowBlock => {
                            stroke_rect(
                                &mut front,
                                width,
                                height,
                                x,
                                y,
                                span,
                                cell_h,
                                palette.cursor,
                            );
                        }
                        CursorShape::Block | CursorShape::Hidden => {}
                    },
                    CursorPaint::Off => {}
                }
            }
        }
        verts.extend(backs);
        verts.extend(front);
        self.build_tab_bar(&mut verts, palette, titles, active, width, height);
        verts
    }

    fn build_tab_bar(
        &mut self,
        verts: &mut Vec<Vertex>,
        palette: &Palette,
        titles: &[String],
        active: usize,
        width: f32,
        height: f32,
    ) {
        let cell_w = self.fonts.cell_width;
        let cell_h = self.fonts.cell_height;
        let tab_w = tab_bar::tab_width(width, cell_w, titles.len());
        let new_x = tab_bar::new_button_x(width, cell_w, titles.len());
        let new_w = (width - new_x).min(3.0 * cell_w).max(0.0);

        // Paint the opaque header after terminal glyphs so their overhangs do
        // not bleed into the tab bar.
        push_solid(
            verts,
            width,
            height,
            0.0,
            0.0,
            width,
            cell_h,
            palette.background,
        );
        for (i, title) in titles.iter().enumerate() {
            let x = i as f32 * tab_w;
            let bg = if i == active {
                palette.selection
            } else {
                palette.background
            };
            push_solid(verts, width, height, x, 0.0, tab_w, cell_h, bg);
            if tab_w <= 0.0 {
                continue;
            }

            // Keep a full cell clear at either edge. A close affordance is
            // only drawn when the shared hit geometry exposes its hit zone.
            let has_close = tab_w >= 2.0 * cell_w;
            let label_start = x + cell_w;
            let label_end = x + tab_w - if has_close { 2.0 * cell_w } else { cell_w };
            if label_end > label_start {
                let max_cells = ((label_end - label_start) / cell_w).floor() as usize;
                for (cell, ch) in title.chars().take(max_cells).enumerate() {
                    let glyph = self.atlas.glyph(&self.fonts, ch, 0);
                    if glyph.w == 0 {
                        continue;
                    }
                    let gx = label_start + cell as f32 * cell_w + glyph.xmin as f32;
                    let gy = self.fonts.ascent - (glyph.ymin as f32 + glyph.h as f32);
                    // Skip overhanging glyphs instead of letting them paint
                    // into a neighboring tab.
                    if gx < label_start || gx + glyph.w as f32 > label_end {
                        continue;
                    }
                    let (u0, v0, u1, v1) = self.atlas.uv(glyph);
                    push_glyph(
                        verts,
                        width,
                        height,
                        gx,
                        gy,
                        glyph.w as f32,
                        glyph.h as f32,
                        u0,
                        v0,
                        u1,
                        v1,
                        palette.foreground,
                    );
                }
            }

            if has_close {
                let cx = x + tab_w - cell_w * 1.5;
                self.push_header_char(verts, 'x', cx, width, height, palette.foreground);
            }
        }
        if new_w > 0.0 {
            push_solid(
                verts,
                width,
                height,
                new_x,
                0.0,
                new_w,
                cell_h,
                palette.background,
            );
            if new_w >= cell_w {
                let cx = new_x + ((new_w - cell_w) * 0.5);
                self.push_header_char(verts, '+', cx, width, height, palette.foreground);
            }
        }
    }

    fn push_header_char(
        &mut self,
        verts: &mut Vec<Vertex>,
        ch: char,
        x: f32,
        width: f32,
        height: f32,
        color: Rgb,
    ) {
        let glyph = self.atlas.glyph(&self.fonts, ch, 0);
        if glyph.w == 0 {
            return;
        }
        let gx = x + (self.fonts.cell_width - glyph.w as f32) * 0.5 + glyph.xmin as f32;
        if gx < x || gx + glyph.w as f32 > x + self.fonts.cell_width {
            return;
        }
        let gy = self.fonts.ascent - (glyph.ymin as f32 + glyph.h as f32);
        let (u0, v0, u1, v1) = self.atlas.uv(glyph);
        push_glyph(
            verts,
            width,
            height,
            gx,
            gy,
            glyph.w as f32,
            glyph.h as f32,
            u0,
            v0,
            u1,
            v1,
            color,
        );
    }

    fn sync_atlas(&mut self) {
        if self.atlas.width != self.atlas_texture.width()
            || self.atlas.height != self.atlas_texture.height()
        {
            let (texture, view) = make_texture(&self.device, self.atlas.width, self.atlas.height);
            self.atlas_texture = texture;
            self.atlas_view = view;
            self.bind_group = make_bind_group(
                &self.device,
                &self.bind_group_layout,
                &self.atlas_view,
                &self.sampler,
            );
            self.atlas.dirty = true;
        }
        if !self.atlas.dirty {
            return;
        }
        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.atlas.pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(self.atlas.width),
                rows_per_image: Some(self.atlas.height),
            },
            wgpu::Extent3d {
                width: self.atlas.width,
                height: self.atlas.height,
                depth_or_array_layers: 1,
            },
        );
        self.atlas.dirty = false;
    }

    fn ensure_vertex_buffer(&mut self, len: usize) {
        if len <= self.vertex_capacity {
            return;
        }
        self.vertex_capacity = len.next_power_of_two().max(4096);
        self.vertex_buf = vertex_buffer(&self.device, self.vertex_capacity);
    }
}

fn style_index(flags: Flags) -> u8 {
    let mut style = 0u8;
    if flags.contains(Flags::BOLD) {
        style |= 1;
    }
    if flags.contains(Flags::ITALIC) {
        style |= 2;
    }
    style
}

fn gpu_color(rgb: Rgb) -> wgpu::Color {
    wgpu::Color {
        r: f64::from(rgb.r) / 255.0,
        g: f64::from(rgb.g) / 255.0,
        b: f64::from(rgb.b) / 255.0,
        a: 1.0,
    }
}

fn rgba(rgb: Rgb) -> [f32; 4] {
    [
        f32::from(rgb.r) / 255.0,
        f32::from(rgb.g) / 255.0,
        f32::from(rgb.b) / 255.0,
        1.0,
    ]
}

#[allow(clippy::too_many_arguments)]
fn push_solid(
    verts: &mut Vec<Vertex>,
    sw: f32,
    sh: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Rgb,
) {
    push_quad(
        verts,
        sw,
        sh,
        x,
        y,
        w,
        h,
        0.0,
        0.0,
        0.0,
        0.0,
        rgba(color),
        0,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_glyph(
    verts: &mut Vec<Vertex>,
    sw: f32,
    sh: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    color: Rgb,
) {
    push_quad(verts, sw, sh, x, y, w, h, u0, v0, u1, v1, rgba(color), 1);
}

#[allow(clippy::too_many_arguments)]
fn stroke_rect(
    verts: &mut Vec<Vertex>,
    sw: f32,
    sh: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: Rgb,
) {
    push_solid(verts, sw, sh, x, y, w, 1.0, color);
    push_solid(verts, sw, sh, x, y + h - 1.0, w, 1.0, color);
    push_solid(verts, sw, sh, x, y, 1.0, h, color);
    push_solid(verts, sw, sh, x + w - 1.0, y, 1.0, h, color);
}

#[allow(clippy::too_many_arguments)]
fn push_quad(
    verts: &mut Vec<Vertex>,
    sw: f32,
    sh: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    u0: f32,
    v0: f32,
    u1: f32,
    v1: f32,
    color: [f32; 4],
    mode: u32,
) {
    if w <= 0.0 || h <= 0.0 || sw <= 0.0 || sh <= 0.0 {
        return;
    }
    let x0 = x / sw * 2.0 - 1.0;
    let x1 = (x + w) / sw * 2.0 - 1.0;
    let y0 = 1.0 - y / sh * 2.0;
    let y1 = 1.0 - (y + h) / sh * 2.0;
    let v = |px, py, u, vv| Vertex {
        pos: [px, py],
        uv: [u, vv],
        color,
        mode,
    };
    verts.extend_from_slice(&[
        v(x0, y0, u0, v0),
        v(x1, y0, u1, v0),
        v(x1, y1, u1, v1),
        v(x0, y0, u0, v0),
        v(x1, y1, u1, v1),
        v(x0, y1, u0, v1),
    ]);
}

#[derive(Clone, Copy)]
struct Glyph {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    xmin: i32,
    ymin: i32,
    fake_bold: bool,
}

struct Atlas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    glyphs: HashMap<(char, u8), Glyph>,
    dirty: bool,
    grew: bool,
}

impl Atlas {
    fn new(size: u32) -> Self {
        Self {
            width: size,
            height: size,
            pixels: vec![0; (size * size) as usize],
            shelf_x: 1,
            shelf_y: 1,
            shelf_h: 0,
            glyphs: HashMap::new(),
            dirty: false,
            grew: false,
        }
    }

    fn reset(&mut self) {
        self.pixels.fill(0);
        self.glyphs.clear();
        self.shelf_x = 1;
        self.shelf_y = 1;
        self.shelf_h = 0;
        self.dirty = true;
    }

    fn glyph(&mut self, fonts: &FontSet, ch: char, style: u8) -> Glyph {
        if let Some(glyph) = self.glyphs.get(&(ch, style)) {
            return *glyph;
        }
        let raster = fonts.rasterize(ch, style);
        self.insert(ch, style, &raster.metrics, &raster.bitmap, raster.fake_bold)
    }

    fn insert(
        &mut self,
        ch: char,
        style: u8,
        metrics: &fontdue::Metrics,
        bitmap: &[u8],
        fake_bold: bool,
    ) -> Glyph {
        let w = metrics.width as u32;
        let h = metrics.height as u32;
        if w == 0 || h == 0 || bitmap.len() < (w * h) as usize {
            let glyph = Glyph {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
                xmin: 0,
                ymin: 0,
                fake_bold,
            };
            self.glyphs.insert((ch, style), glyph);
            return glyph;
        }
        let gw = w + 1;
        let gh = h + 1;
        if self.shelf_x + gw > self.width {
            self.shelf_y += self.shelf_h;
            self.shelf_x = 1;
            self.shelf_h = 0;
        }
        if self.shelf_y + gh > self.height && !self.grow() {
            let glyph = Glyph {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
                xmin: metrics.xmin,
                ymin: metrics.ymin,
                fake_bold,
            };
            self.glyphs.insert((ch, style), glyph);
            return glyph;
        }
        if self.shelf_x + gw > self.width {
            self.shelf_y += self.shelf_h;
            self.shelf_x = 1;
            self.shelf_h = 0;
        }
        let x = self.shelf_x;
        let y = self.shelf_y;
        if y + h > self.height || x + w > self.width {
            let glyph = Glyph {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
                xmin: metrics.xmin,
                ymin: metrics.ymin,
                fake_bold,
            };
            self.glyphs.insert((ch, style), glyph);
            return glyph;
        }
        for row in 0..h {
            let src = (row * w) as usize;
            let dst = ((y + row) * self.width + x) as usize;
            self.pixels[dst..dst + w as usize].copy_from_slice(&bitmap[src..src + w as usize]);
        }
        self.shelf_x += gw;
        self.shelf_h = self.shelf_h.max(gh);
        self.dirty = true;
        let glyph = Glyph {
            x,
            y,
            w,
            h,
            xmin: metrics.xmin,
            ymin: metrics.ymin,
            fake_bold,
        };
        self.glyphs.insert((ch, style), glyph);
        glyph
    }

    fn grow(&mut self) -> bool {
        let width = (self.width * 2).min(ATLAS_MAX);
        let height = (self.height * 2).min(ATLAS_MAX);
        if width == self.width && height == self.height {
            return false;
        }
        let mut pixels = vec![0u8; (width * height) as usize];
        for row in 0..self.height {
            let src = (row * self.width) as usize;
            let dst = (row * width) as usize;
            pixels[dst..dst + self.width as usize]
                .copy_from_slice(&self.pixels[src..src + self.width as usize]);
        }
        self.pixels = pixels;
        self.width = width;
        self.height = height;
        self.dirty = true;
        self.grew = true;
        true
    }

    fn uv(&self, glyph: Glyph) -> (f32, f32, f32, f32) {
        let w = self.width as f32;
        let h = self.height as f32;
        (
            glyph.x as f32 / w,
            glyph.y as f32 / h,
            (glyph.x + glyph.w) as f32 / w,
            (glyph.y + glyph.h) as f32 / h,
        )
    }
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}

fn init_gpu(window: Arc<Window>, width: u32, height: u32) -> Result<Gpu, String> {
    let attempts = [
        ("primary", wgpu::Backends::PRIMARY),
        ("gl", wgpu::Backends::GL),
        ("all", wgpu::Backends::all()),
    ];
    let mut errors = Vec::new();
    for (name, backends) in attempts {
        match init_backend(window.clone(), width, height, backends) {
            Ok(gpu) => return Ok(gpu),
            Err(err) => errors.push(format!("{name}: {err}")),
        }
    }
    Err(format!("no gpu backend ({})", errors.join("; ")))
}

fn init_backend(
    window: Arc<Window>,
    width: u32,
    height: u32,
    backends: wgpu::Backends,
) -> Result<Gpu, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        flags: wgpu::InstanceFlags::default(),
        dx12_shader_compiler: wgpu::Dx12Compiler::default(),
        gles_minor_version: wgpu::Gles3MinorVersion::Automatic,
    });
    let surface = instance
        .create_surface(window)
        .map_err(|err| format!("surface: {err}"))?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
    }))
    .ok_or_else(|| "no adapter".to_string())?;

    let info = adapter.get_info();
    eprintln!("aiterm: gpu {} ({:?})", info.name, info.backend);

    let (device, queue) = request_device(&adapter)?;
    let caps = surface.get_capabilities(&adapter);
    if caps.formats.is_empty() {
        return Err("surface has no formats".to_string());
    }
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|format| {
            matches!(
                format,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        })
        .or_else(|| {
            caps.formats
                .iter()
                .copied()
                .find(|format| !format.is_srgb())
        })
        .unwrap_or(caps.formats[0]);
    let alpha = caps
        .alpha_modes
        .iter()
        .copied()
        .find(|mode| *mode == wgpu::CompositeAlphaMode::Opaque)
        .unwrap_or(caps.alpha_modes[0]);
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width,
        height,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: alpha,
        view_formats: vec![],
    };
    surface.configure(&device, &config);
    Ok(Gpu {
        surface,
        device,
        queue,
        config,
    })
}

fn request_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue), String> {
    let mut last = String::from("device request failed");
    for downlevel in [false, true] {
        let required_limits = if downlevel {
            wgpu::Limits::downlevel_defaults()
        } else {
            wgpu::Limits::default()
        };
        let desc = wgpu::DeviceDescriptor {
            label: Some("aiterm"),
            required_features: wgpu::Features::empty(),
            required_limits,
            memory_hints: wgpu::MemoryHints::MemoryUsage,
        };
        match pollster::block_on(adapter.request_device(&desc, None)) {
            Ok(pair) => return Ok(pair),
            Err(err) => last = err.to_string(),
        }
    }
    Err(last)
}

fn make_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("atlas"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn make_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("atlas"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn make_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("term"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("term"),
        bind_group_layouts: &[layout],
        push_constant_ranges: &[],
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("term"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
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
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 16,
                        shader_location: 2,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Uint32,
                        offset: 32,
                        shader_location: 3,
                    },
                ],
            }],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview: None,
        cache: None,
    })
}

fn vertex_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("verts"),
        size: (capacity * std::mem::size_of::<Vertex>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

const SHADER: &str = r#"
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) mode: u32,
};

@vertex
fn vs_main(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) mode: u32,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    out.mode = mode;
    return out;
}

@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var atlas_sampler: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let coverage = textureSample(atlas, atlas_sampler, in.uv).r;
    let alpha = select(1.0, coverage, in.mode == 1u);
    return vec4<f32>(in.color.rgb, in.color.a * alpha);
}
"#;
