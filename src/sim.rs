// Sprite simulation. Each worker owns a chunk of sprites and steps them
// independently; per-frame snapshots are sent to the main thread for rendering
// and overlap detection.

use bytemuck::{Pod, Zeroable};

/// Logical world space is [-1, 1] on both axes (NDC-ish).
pub const WORLD_MIN: f32 = -1.0;
pub const WORLD_MAX: f32 = 1.0;

#[derive(Clone, Copy, Debug)]
pub struct Sprite {
    pub id: u32,
    pub atlas_index: u32, // index into atlas::SPRITES
    pub pos: [f32; 2],
    pub vel: [f32; 2],
    /// Half-extent in world units (square).
    pub half_size: f32,
}

/// One quad = 4 vertices. We send these straight to the GPU.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug)]
pub struct Vertex {
    pub pos: [f32; 2],
    pub uv: [f32; 2],
}

/// A per-tick snapshot from a worker.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub worker_id: u32,
    /// Tick counter — strictly increasing, used by main to discard stale
    /// snapshots if needed.
    pub tick: u64,
    /// One entry per sprite this worker owns.
    pub sprites: Vec<SpriteState>,
    /// 4 vertices per sprite, in the same order as `sprites`.
    pub verts: Vec<Vertex>,
}

#[derive(Clone, Copy, Debug)]
pub struct SpriteState {
    pub id: u32,
    pub pos: [f32; 2],
    pub half_size: f32,
}

pub fn step(sprites: &mut [Sprite], dt: f32) {
    for s in sprites.iter_mut() {
        s.pos[0] += s.vel[0] * dt;
        s.pos[1] += s.vel[1] * dt;

        // Bounce off the world edges.
        if s.pos[0] - s.half_size < WORLD_MIN {
            s.pos[0] = WORLD_MIN + s.half_size;
            s.vel[0] = s.vel[0].abs();
        } else if s.pos[0] + s.half_size > WORLD_MAX {
            s.pos[0] = WORLD_MAX - s.half_size;
            s.vel[0] = -s.vel[0].abs();
        }
        if s.pos[1] - s.half_size < WORLD_MIN {
            s.pos[1] = WORLD_MIN + s.half_size;
            s.vel[1] = s.vel[1].abs();
        } else if s.pos[1] + s.half_size > WORLD_MAX {
            s.pos[1] = WORLD_MAX - s.half_size;
            s.vel[1] = -s.vel[1].abs();
        }
    }
}

/// Build 4 vertices for a sprite at `s.pos` with UVs from the atlas rect.
pub fn build_quad(s: &Sprite, uv: [f32; 4]) -> [Vertex; 4] {
    let x0 = s.pos[0] - s.half_size;
    let x1 = s.pos[0] + s.half_size;
    let y0 = s.pos[1] - s.half_size;
    let y1 = s.pos[1] + s.half_size;
    let [u0, v0, u1, v1] = uv;
    [
        Vertex { pos: [x0, y0], uv: [u0, v1] },
        Vertex { pos: [x1, y0], uv: [u1, v1] },
        Vertex { pos: [x1, y1], uv: [u1, v0] },
        Vertex { pos: [x0, y1], uv: [u0, v0] },
    ]
}
