// Worker pool — spawns N background workers via `web_thread` (a drop-in for
// `std::thread` that uses Web Workers under the hood when targeting wasm).
//
// Each worker owns a partition of sprites, simulates them on its own clock,
// and sends per-tick `Snapshot`s through a crossbeam-channel back to the main
// thread. The main thread renders + does cross-partition overlap detection.

use crossbeam_channel::{bounded, Receiver, Sender};

use crate::atlas;
use crate::sim::{self, Snapshot, Sprite, SpriteState};

pub const NUM_WORKERS: usize = 4;
/// Worker tick rate, in ticks per second.
const WORKER_TPS: f32 = 60.0;

pub struct WorkerPool {
    /// One receiver per worker — main thread `try_recv()`s on each frame.
    pub rx: Vec<Receiver<Snapshot>>,
}

pub fn spawn_pool(atlas_w: f32, atlas_h: f32) -> WorkerPool {
    // One sprite per atlas entry, partitioned across NUM_WORKERS workers.
    let total = atlas::SPRITES.len();
    let mut rx = Vec::with_capacity(NUM_WORKERS);
    for worker_id in 0..NUM_WORKERS {
        let start = (total * worker_id) / NUM_WORKERS;
        let end = (total * (worker_id + 1)) / NUM_WORKERS;
        let indices: Vec<u32> = (start..end).map(|i| i as u32).collect();
        let (tx, rxc): (Sender<Snapshot>, Receiver<Snapshot>) = bounded(2);
        rx.push(rxc);
        if indices.is_empty() {
            continue;
        }
        let sprites = make_sprites(worker_id as u32, &indices);
        let uvs = make_uvs(&sprites, atlas_w, atlas_h);
        wasm_thread::spawn(move || worker_main(worker_id as u32, sprites, uvs, tx));
    }
    WorkerPool { rx }
}

fn worker_main(
    worker_id: u32,
    mut sprites: Vec<Sprite>,
    uvs: Vec<[f32; 4]>,
    tx: Sender<Snapshot>,
) {
    let dt = 1.0 / WORKER_TPS;
    let mut tick: u64 = 0;
    loop {
        sim::step(&mut sprites, dt);

        let mut verts = Vec::with_capacity(sprites.len() * 4);
        let mut states = Vec::with_capacity(sprites.len());
        for (s, uv) in sprites.iter().zip(uvs.iter()) {
            verts.extend_from_slice(&sim::build_quad(s, *uv));
            states.push(SpriteState {
                id: s.id,
                pos: s.pos,
                half_size: s.half_size,
            });
        }
        let snap = Snapshot {
            worker_id,
            tick,
            sprites: states,
            verts,
        };
        // If the receiver has dropped (main is shutting down) bail out.
        if tx.send(snap).is_err() {
            break;
        }
        tick += 1;

        // Sleep this worker's tick interval. On wasm, web-thread's `sleep`
        // hands control back to the worker's event loop; on native it just
        // calls thread::sleep.
        wasm_thread::sleep(std::time::Duration::from_secs_f32(dt));
    }
}

fn make_sprites(worker_id: u32, atlas_indices: &[u32]) -> Vec<Sprite> {
    // Use fastrand with a worker-derived seed so each worker gets different
    // initial positions/velocities. fastrand is wasm-friendly (uses getrandom).
    let mut rng = fastrand::Rng::with_seed(0xC0FFEE_u64.wrapping_mul((worker_id as u64) + 1));
    atlas_indices
        .iter()
        .enumerate()
        .map(|(i, &atlas_index)| {
            let id = worker_id * 1000 + i as u32;
            let pos = [rng.f32() * 1.6 - 0.8, rng.f32() * 1.6 - 0.8];
            let angle = rng.f32() * std::f32::consts::TAU;
            let speed = 0.15 + rng.f32() * 0.35;
            let vel = [angle.cos() * speed, angle.sin() * speed];
            Sprite {
                id,
                atlas_index,
                pos,
                vel,
                half_size: 0.07,
            }
        })
        .collect()
}

fn make_uvs(sprites: &[Sprite], atlas_w: f32, atlas_h: f32) -> Vec<[f32; 4]> {
    sprites
        .iter()
        .map(|s| {
            let r = &atlas::SPRITES[s.atlas_index as usize];
            let u0 = r.x as f32 / atlas_w;
            let v0 = r.y as f32 / atlas_h;
            let u1 = (r.x + r.w) as f32 / atlas_w;
            let v1 = (r.y + r.h) as f32 / atlas_h;
            [u0, v0, u1, v1]
        })
        .collect()
}

/// Pull the latest snapshot from each worker (drains the channel so we always
/// see the most recent tick). Strictly non-blocking — main thread MUST never
/// `recv()` on these channels.
pub fn poll_latest(pool: &WorkerPool, latest: &mut Vec<Option<Snapshot>>) {
    if latest.len() != pool.rx.len() {
        latest.resize_with(pool.rx.len(), || None);
    }
    for (i, rx) in pool.rx.iter().enumerate() {
        while let Ok(snap) = rx.try_recv() {
            latest[i] = Some(snap);
        }
    }
}

/// Detect newly-overlapping sprite pairs across all workers' latest snapshots.
/// `prev_overlaps` is updated in place — pairs that were not overlapping last
/// frame but are now are returned for the audio side to play a pling.
pub fn detect_new_overlaps(
    snapshots: &[Option<Snapshot>],
    prev_overlaps: &mut std::collections::HashSet<(u32, u32)>,
) -> Vec<(u32, u32)> {
    let mut all: Vec<&SpriteState> = Vec::new();
    for s in snapshots.iter().flatten() {
        all.extend(s.sprites.iter());
    }
    let mut current: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    let mut new_events = Vec::new();

    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            let a = all[i];
            let b = all[j];
            let dx = (a.pos[0] - b.pos[0]).abs();
            let dy = (a.pos[1] - b.pos[1]).abs();
            let r = a.half_size + b.half_size;
            if dx < r && dy < r {
                let key = if a.id < b.id { (a.id, b.id) } else { (b.id, a.id) };
                if !prev_overlaps.contains(&key) {
                    new_events.push(key);
                }
                current.insert(key);
            }
        }
    }
    *prev_overlaps = current;
    new_events
}
