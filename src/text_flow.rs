/// A horizontal segment within a text frame where text may be placed.
/// Coordinates are in pixels, relative to the text frame's top-left corner.
#[derive(Debug, Clone)]
pub struct Interval {
    pub x: f64,
    pub width: f64,
}

/// Provides writable intervals for each line of a text frame.
/// Pre-computed once per shape change; queried once per line during render.
pub trait TextFlowProvider {
    /// Returns the intervals available at vertical position `y_px` for a line
    /// of height `line_h_px`. Both values are in frame-local pixels.
    fn intervals_for_line(&self, y_px: f64, line_h_px: f64) -> &[Interval];
}

// ── PrecomputedFlowProvider ───────────────────────────────────────────────────

/// Flow provider backed by a pre-scanned interval table (one row per pixel row).
#[derive(Clone)]
pub struct PrecomputedFlowProvider {
    /// rows[y] = list of writable intervals on pixel row y.
    rows: Vec<Vec<Interval>>,
}

impl PrecomputedFlowProvider {
    /// Build from rectangular obstacles in frame-local pixel space.
    /// `obstacles`: list of `(rel_x, rel_y, w, h)` in pixels.
    /// `padding_px`: safety margin eroded from each obstacle edge.
    #[cfg(test)]
    pub fn from_rect_obstacles(
        frame_w_px: usize,
        frame_h_px: usize,
        obstacles: &[(f64, f64, f64, f64)],
        padding_px: f64,
    ) -> Self {
        let fw = frame_w_px as f64;
        let rows = (0..frame_h_px).map(|y| {
            let yf = y as f64;
            let mut intervals = vec![Interval { x: 0.0, width: fw }];
            for &(ox, oy, ow, oh) in obstacles {
                if yf >= oy && yf < oy + oh {
                    let sub_start = (ox - padding_px).max(0.0);
                    let sub_end   = (ox + ow + padding_px).min(fw);
                    intervals = subtract_range(&intervals, sub_start, sub_end);
                }
            }
            intervals
        }).collect();
        Self { rows }
    }

    /// Build from an A8 mask surface (general case for SVG / shaped PNG).
    /// `data`: raw A8 bytes (one byte per pixel, non-zero = writable).
    /// `padding_px`: safety margin eroded from writable zone edges.
    pub fn from_a8_mask(data: &[u8], w: usize, h: usize, padding_px: f64) -> Self {
        let rows = (0..h).map(|y| {
            scan_row(&data[y * w..(y + 1) * w], w, padding_px)
        }).collect();
        Self { rows }
    }
}

impl TextFlowProvider for PrecomputedFlowProvider {
    fn intervals_for_line(&self, y_px: f64, line_h_px: f64) -> &[Interval] {
        // Sample at the vertical midpoint of the line for best accuracy.
        let mid = (y_px + line_h_px * 0.5) as usize;
        let idx = mid.min(self.rows.len().saturating_sub(1));
        &self.rows[idx]
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Remove the horizontal range `[sub_start, sub_end)` from a list of intervals.
#[cfg(test)]
fn subtract_range(intervals: &[Interval], sub_start: f64, sub_end: f64) -> Vec<Interval> {
    if sub_start >= sub_end { return intervals.to_vec(); }
    let mut result = Vec::new();
    for iv in intervals {
        let iv_end = iv.x + iv.width;
        // Left remnant: iv.x .. sub_start
        if iv.x < sub_start {
            let w = (sub_start - iv.x).min(iv.width);
            if w > 0.0 { result.push(Interval { x: iv.x, width: w }); }
        }
        // Right remnant: sub_end .. iv_end
        if iv_end > sub_end {
            let x = sub_end.max(iv.x);
            let w = iv_end - x;
            if w > 0.0 { result.push(Interval { x, width: w }); }
        }
    }
    result
}

/// Scan one A8 pixel row and return writable intervals (with `padding_px` eroded).
fn scan_row(row: &[u8], w: usize, padding_px: f64) -> Vec<Interval> {
    let mut intervals = Vec::new();
    let mut start: Option<usize> = None;
    for x in 0..w {
        let writable = row[x] > 128;
        match (start, writable) {
            (None, true)  => { start = Some(x); }
            (Some(s), false) => {
                push_padded(&mut intervals, s as f64, x as f64, padding_px);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        push_padded(&mut intervals, s as f64, w as f64, padding_px);
    }
    intervals
}

fn push_padded(out: &mut Vec<Interval>, raw_x: f64, raw_end: f64, pad: f64) {
    let x = raw_x + pad;
    let w = raw_end - pad - x;
    if w > 0.0 { out.push(Interval { x, width: w }); }
}

#[cfg(test)]
mod tests {
    use super::{PrecomputedFlowProvider, TextFlowProvider};

    #[test]
    fn rectangular_obstacle_subtracts_available_interval() {
        let provider = PrecomputedFlowProvider::from_rect_obstacles(
            10,
            5,
            &[(2.0, 1.0, 4.0, 2.0)],
            0.0,
        );

        let row = provider.intervals_for_line(1.0, 1.0);
        assert_eq!(row.len(), 2);
        assert_eq!((row[0].x, row[0].width), (0.0, 2.0));
        assert_eq!((row[1].x, row[1].width), (6.0, 4.0));
    }

    #[test]
    fn padding_erodes_interval_edges() {
        let provider = PrecomputedFlowProvider::from_rect_obstacles(
            10,
            4,
            &[(3.0, 0.0, 2.0, 4.0)],
            1.0,
        );

        let row = provider.intervals_for_line(1.0, 1.0);
        assert_eq!(row.len(), 2);
        assert_eq!((row[0].x, row[0].width), (0.0, 2.0));
        assert_eq!((row[1].x, row[1].width), (6.0, 4.0));
    }

    #[test]
    fn fully_blocked_row_has_no_intervals() {
        let provider = PrecomputedFlowProvider::from_rect_obstacles(
            8,
            3,
            &[(0.0, 0.0, 8.0, 3.0)],
            0.0,
        );

        assert!(provider.intervals_for_line(1.0, 1.0).is_empty());
    }

    #[test]
    fn fully_writable_mask_row_stays_available() {
        let mask = vec![255u8; 12];
        let provider = PrecomputedFlowProvider::from_a8_mask(&mask, 4, 3, 0.0);

        let row = provider.intervals_for_line(1.0, 1.0);
        assert_eq!(row.len(), 1);
        assert_eq!((row[0].x, row[0].width), (0.0, 4.0));
    }
}
