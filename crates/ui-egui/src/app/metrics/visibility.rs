//! Diagnostic geometry only: rendering remains the original open cubic.
//! See docs/development.md and the #371 decision for the conservative envelope contract.
use egui::{
    Pos2, Rect,
    epaint::{CubicBezierShape, TessellationOptions},
};

#[cfg(test)]
std::thread_local! { static CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(in crate::app) fn take_calls() -> usize {
    CALLS.with(|n| n.replace(0))
}

pub(in crate::app) struct Visibility {
    clip: Rect,
    tolerance: f32,
    feathering: f32,
}
impl Visibility {
    pub(in crate::app) fn new(clip: Rect, pixels_per_point: f32, options: &TessellationOptions) -> Option<Self> {
        let tolerance = options.bezier_tolerance;
        if !clip.is_finite() || !pixels_per_point.is_finite() || pixels_per_point <= 0.0 || !tolerance.is_finite() || tolerance <= 0.0 {
            return None;
        }
        let feathering = if options.feathering { options.feathering_size_in_pixels / pixels_per_point } else { 0.0 };
        // The upstream cubic callback divides its tolerance into 0.2 and 0.8
        // portions. Reject an underflowed/overflowed numeric configuration before it.
        let quadratic_tolerance = tolerance * 0.2;
        let denominator = 432.0 * quadratic_tolerance * quadratic_tolerance;
        if !feathering.is_finite() || feathering < 0.0 || !denominator.is_finite() || denominator <= 0.0 {
            return None;
        }
        Some(Self { clip, tolerance, feathering })
    }
    pub(in crate::app) fn intersects(&self, points: [Pos2; 4], width: f32) -> Option<bool> {
        if !points.iter().all(|p| p.is_finite()) || !width.is_finite() || width <= 0.0 {
            return None;
        }
        // An empty parent clip must stay empty; expanding it could otherwise create
        // a spurious visible rectangle when the inverted gap is narrower than the stroke.
        if self.clip.is_negative() {
            return Some(false);
        }
        #[cfg(test)]
        CALLS.with(|n| n.set(n.get().saturating_add(1)));
        // OPEN epaint0.36.2 joins cut sharp corners: normal length <=2. Non-AA radius
        // width/2, thin-AA radius f, thick-AA radius (width+f)/2 plus endpoint f.
        // Thus width+2f encloses their coordinate offsets. Tolerance is a numerical
        // allowance; this axis envelope is conservative, not triangle/pixel exact.
        let margin = width + 2.0 * self.feathering + self.tolerance;
        let clip = self.clip.expand(margin);
        if !margin.is_finite() || !clip.is_finite() {
            return None;
        }
        // flatten_closed seeds points[0] before this callback for an OPEN cubic.
        let mut previous = *points.first()?;
        let mut visible = clip.contains(previous);
        let mut valid = true;
        let curve = CubicBezierShape::from_points_stroke(points, false, egui::Color32::TRANSPARENT, egui::Stroke::NONE);
        // Finite endpoints alone do not guarantee finite upstream subdivision arithmetic.
        // This is a representation check, not an additional subdivision/work quota.
        let [a, b, c, d] = points;
        let error = a.to_vec2() - 3.0 * b.to_vec2() + 3.0 * c.to_vec2() - d.to_vec2();
        let denominator = 432.0 * (self.tolerance * 0.2) * (self.tolerance * 0.2);
        if !(error.length_sq() / denominator).is_finite() {
            return None;
        }
        curve.for_each_flattened_with_t(self.tolerance, &mut |point, _| {
            if !point.is_finite() {
                valid = false;
            } else if valid {
                visible |= segment_intersects(previous, point, clip);
            }
            previous = point;
        });
        valid.then_some(visible)
    }
}

/// Closed segment versus closed rectangle, with f64 intermediate arithmetic. No point Vec.
fn segment_intersects(a: Pos2, b: Pos2, clip: Rect) -> bool {
    if clip.is_negative() {
        return false;
    }
    let mut lo = 0.0f64;
    let mut hi = 1.0f64;
    for (start, end, min, max) in [(a.x, b.x, clip.min.x, clip.max.x), (a.y, b.y, clip.min.y, clip.max.y)] {
        let start = f64::from(start);
        let delta = f64::from(end) - start;
        if delta == 0.0 {
            if start < f64::from(min) || start > f64::from(max) {
                return false;
            }
        } else {
            let t0 = (f64::from(min) - start) / delta;
            let t1 = (f64::from(max) - start) / delta;
            lo = lo.max(t0.min(t1));
            hi = hi.min(t0.max(t1));
            if lo > hi {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests;
