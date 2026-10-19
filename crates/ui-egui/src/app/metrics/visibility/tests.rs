use super::*;
fn line(a: Pos2, b: Pos2) -> [Pos2; 4] {
    [a, a + (b - a) / 3.0, a + (b - a) * (2.0 / 3.0), b]
}
fn config(clip: Rect) -> Visibility {
    Visibility::new(clip, 1.0, &TessellationOptions::default()).unwrap()
}
fn rect() -> Rect {
    Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 100.0))
}
#[test]
fn crossings_first_segment_hull_false_positive_and_stroke_boundaries() {
    let v = config(rect());
    assert_eq!(v.intersects(line(Pos2::new(-50.0, 50.0), Pos2::new(150.0, 50.0)), 2.0), Some(true));
    assert_eq!(v.intersects(line(Pos2::new(-50.0, -50.0), Pos2::new(150.0, -50.0)), 2.0), Some(false));
    // All four points' rectangle overlaps clip, but the bowed curve never reaches it.
    let bowed = [Pos2::new(-50.0, -50.0), Pos2::new(150.0, -50.0), Pos2::new(150.0, -50.0), Pos2::new(150.0, 150.0)];
    let tiny = Rect::from_min_max(Pos2::new(0.0, 70.0), Pos2::new(10.0, 80.0));
    assert!(Rect::from_points(&bowed).intersects(tiny));
    assert_eq!(config(tiny).intersects(bowed, 1.0), Some(false));
    assert_eq!(v.intersects(line(Pos2::new(-20.0, -3.0), Pos2::new(120.0, -3.0)), 2.0), Some(true), "stroke/AA-only envelope");
    assert_eq!(v.intersects(line(Pos2::new(-20.0, -8.0), Pos2::new(120.0, -8.0)), 2.0), Some(false));
    assert_eq!(v.intersects(line(Pos2::new(-20.0, 0.0), Pos2::new(120.0, 0.0)), 2.0), Some(true), "tangent");
    assert_eq!(v.intersects([Pos2::new(50.0, 50.0); 4], 1.0), Some(true), "initial point/degenerate cubic");
    let straight = line(Pos2::new(-100.0, 50.0), Pos2::new(1000.0, 50.0));
    let mut flattened = Vec::new();
    let c = CubicBezierShape::from_points_stroke(straight, false, egui::Color32::TRANSPARENT, egui::Stroke::NONE);
    c.for_each_flattened_with_t(v.tolerance, &mut |p, _| flattened.push(p));
    assert_eq!(flattened.len(), 1, "straight callback only emits the END, so this crossing needs seeded initial point");
    assert_eq!(v.intersects(straight, 1.0), Some(true));
    let near = Rect::from_min_max(Pos2::new(-3.0, -3.0), Pos2::new(-2.5, -2.5));
    assert_eq!(config(near).intersects(line(Pos2::ZERO, Pos2::new(100.0, 0.0)), 1.0), Some(true), "documented corner-envelope overcount");
}
#[test]
fn clip_ppp_halo_and_invalid_inputs_are_explicit() {
    let empty = Rect::from_min_max(Pos2::new(51.0, 51.0), Pos2::new(50.0, 50.0));
    assert_eq!(config(empty).intersects([Pos2::new(50.5, 50.5); 4], 8.0), Some(false), "empty actual clip cannot become visible through stroke expansion");
    let narrow = Rect::from_min_max(Pos2::new(40.0, 40.0), Pos2::new(60.0, 60.0));
    let v = config(narrow);
    let c = line(Pos2::new(0.0, 20.0), Pos2::new(100.0, 20.0));
    assert_eq!(v.intersects(c, 1.0), Some(false));
    assert_eq!(v.intersects(c, 20.0), Some(true));
    for ppp in [1.0, 2.0] {
        assert_eq!(
            Visibility::new(rect(), ppp, &TessellationOptions::default()).unwrap().intersects(line(Pos2::new(-10.0, 50.0), Pos2::new(110.0, 50.0)), 2.0),
            Some(true)
        );
    }
    for ppp in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(Visibility::new(rect(), ppp, &TessellationOptions::default()).is_none());
    }
    for tolerance in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::from_bits(1), f32::MAX] {
        let options = TessellationOptions { bezier_tolerance: tolerance, ..Default::default() };
        assert!(Visibility::new(rect(), 1.0, &options).is_none());
    }
    let mut bad = c;
    bad[1].x = f32::NAN;
    assert_eq!(v.intersects(bad, 2.0), None);
    assert_eq!(v.intersects(c, f32::INFINITY), None);
    assert_eq!(
        v.intersects([Pos2::new(f32::MAX, f32::MAX), Pos2::new(-f32::MAX, 0.0), Pos2::ZERO, Pos2::ZERO], 1.0),
        None,
        "finite coordinates with overflowing subdivision arithmetic refuse before callback"
    );
}
#[test]
fn actual_open_tessellated_vertices_fit_the_streamed_envelope() {
    use egui::epaint::{Mesh, Tessellator};
    let cases = [
        line(Pos2::new(0.0, 0.0), Pos2::new(100.0, 0.0)),
        [Pos2::ZERO, Pos2::new(100.0, 0.0), Pos2::new(-100.0, 0.0), Pos2::new(10.0, 0.0)],
        [Pos2::ZERO, Pos2::new(300.0, 300.0), Pos2::new(-300.0, 300.0), Pos2::new(1.0, 0.0)],
        [Pos2::new(50.0, 50.0); 4],
    ];
    for points in cases {
        for ppp in [1.0, 2.0] {
            for feathering in [false, true] {
                for width in [0.2, 2.0, 8.0] {
                    let options = TessellationOptions { feathering, ..Default::default() };
                    let shape = CubicBezierShape::from_points_stroke(points, false, egui::Color32::TRANSPARENT, egui::Stroke::new(width, egui::Color32::WHITE));
                    let mut mesh = Mesh::default();
                    Tessellator::new(ppp, options, [1, 1], vec![]).tessellate_cubic_bezier(&shape, &mut mesh);
                    for triangle in mesh.indices.as_chunks::<3>().0 {
                        let a = mesh.vertices[triangle[0] as usize].pos;
                        let b = mesh.vertices[triangle[1] as usize].pos;
                        let c = mesh.vertices[triangle[2] as usize].pos;
                        let center = Pos2::new((a.x + b.x + c.x) / 3.0, (a.y + b.y + c.y) / 3.0);
                        let clip = Rect::from_center_size(center, egui::Vec2::splat(0.02));
                        assert_eq!(Visibility::new(clip, ppp, &options).unwrap().intersects(points, width), Some(true));
                    }
                    for vertex in &mesh.vertices {
                        let clip = Rect::from_center_size(vertex.pos, egui::Vec2::splat(0.02));
                        assert_eq!(
                            Visibility::new(clip, ppp, &options).unwrap().intersects(points, width),
                            Some(true),
                            "rendered vertex {:?}, width{width} ppp{ppp} feather{feathering}",
                            vertex.pos
                        );
                    }
                }
            }
        }
    }
}
