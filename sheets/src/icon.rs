/// Procedurally drawn application icon (RGBA, `size` x `size`): a green tile with a white grid.
/// Shared by build.rs (exe .ico) and the app (window icon).
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let radius = s * 0.18;
    let inside_round = |x: f32, y: f32| -> bool {
        let cx = x.clamp(radius, s - radius);
        let cy = y.clamp(radius, s - radius);
        (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
    };
    let grid_l = s * 0.22;
    let grid_r = s * 0.78;
    let grid_t = s * 0.24;
    let grid_b = s * 0.76;
    let line = (s / 24.0).max(1.0);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if !inside_round(fx, fy) {
                continue;
            }
            // vertical gradient green
            let t = fy / s;
            let mut c = [(0x21 as f32 * (1.0 - t) + 0x14 as f32 * t) as u8, (0x9A as f32 * (1.0 - t) + 0x6E as f32 * t) as u8, (0x5B as f32 * (1.0 - t) + 0x3E as f32 * t) as u8];
            if fx >= grid_l && fx <= grid_r && fy >= grid_t && fy <= grid_b {
                let gx = (fx - grid_l) / (grid_r - grid_l) * 3.0;
                let gy = (fy - grid_t) / (grid_b - grid_t) * 4.0;
                let on_v = (gx - gx.round()).abs() * (grid_r - grid_l) / 3.0 < line * 0.5;
                let on_h = (gy - gy.round()).abs() * (grid_b - grid_t) / 4.0 < line * 0.5;
                let header = gy < 1.0;
                if on_v || on_h {
                    c = [255, 255, 255];
                } else if header {
                    c = [0xD8, 0xF0, 0xE2];
                } else {
                    c = [0xFF, 0xFF, 0xFF];
                    if !(on_v || on_h) {
                        c = [0x2E, 0xA0, 0x66];
                    }
                }
            }
            let i = ((y * size + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    px
}
