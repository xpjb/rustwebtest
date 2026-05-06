// Generated atlas metadata + PNG bytes.
include!(concat!(env!("OUT_DIR"), "/atlas_data.rs"));

pub fn find(name: &str) -> Option<&'static SpriteRect> {
    SPRITES.iter().find(|s| s.name == name)
}
