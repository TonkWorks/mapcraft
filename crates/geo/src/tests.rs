use super::*;

fn view(p: Projection) -> View {
    View::new([0.0, 0.0], 0.0, p, 1920.0, 1080.0)
}

#[test]
fn centre_projects_to_frame_centre() {
    for p in Projection::ALL {
        let v = View { center: [30.5, 50.4], zoom: 3.0, ..view(p) };
        let q = v.project([30.5, 50.4]);
        assert!(q.visible);
        assert!((q.xy[0] - 960.0).abs() < 1e-6 && (q.xy[1] - 540.0).abs() < 1e-6, "{p:?} {q:?}");
    }
}

#[test]
fn zoom_zero_fits_the_world_width() {
    let v = view(Projection::Equirectangular);
    let w = v.project([-179.999, 0.0]).xy[0];
    let e = v.project([179.999, 0.0]).xy[0];
    assert!((e - w - 1920.0).abs() < 0.2, "{w} {e}");
    // North is up.
    assert!(v.project([0.0, 45.0]).xy[1] < 540.0);
}

#[test]
fn unproject_inverts_project() {
    for p in Projection::ALL {
        let v = View { center: [10.0, 40.0], zoom: 2.0, rotation: 15.0, ..view(p) };
        for ll in [[12.0, 41.0], [-5.0, 30.0], [25.0, 55.0]] {
            let xy = v.project(ll).xy;
            let back = v.unproject(xy).unwrap();
            assert!((back[0] - ll[0]).abs() < 1e-6 && (back[1] - ll[1]).abs() < 1e-6, "{p:?} {ll:?} -> {back:?}");
        }
    }
}

#[test]
fn globe_hides_the_far_side() {
    let v = view(Projection::Orthographic);
    assert!(v.project([10.0, 0.0]).visible);
    let far = v.project([180.0, 0.0]);
    assert!(!far.visible);
    // Pulled onto the horizon: at the globe's radius from the centre.
    let r = (far.xy[0] - 960.0).hypot(far.xy[1] - 540.0);
    assert!((r - v.scale()).abs() < 1e-6 || r < 1e-6);
    // A ring entirely on the far side is not drawn.
    assert!(v.project_path(&[[170.0, -5.0], [175.0, -5.0], [175.0, 5.0]], true).is_none());
}

#[test]
fn flat_rings_keep_one_longitude_copy() {
    // A ring straddling 180° near a view centred at 170°: no point jumps a world width.
    let v = View { center: [170.0, 0.0], zoom: 2.0, ..view(Projection::Equirectangular) };
    let pts = v.project_path(&[[175.0, 0.0], [179.0, 0.0], [179.0, 5.0]], true).unwrap();
    let xs: Vec<f64> = pts.iter().map(|p| p[0]).collect();
    let spread = xs.iter().cloned().fold(f64::MIN, f64::max) - xs.iter().cloned().fold(f64::MAX, f64::min);
    assert!(spread < 500.0, "{xs:?}");
}

#[test]
fn hostile_views_do_not_produce_nan() {
    let v = View { center: [f64::NAN, f64::INFINITY], zoom: f64::NAN, rotation: f64::NAN, ..view(Projection::Orthographic) };
    let q = v.project([0.0, 0.0]);
    assert!(q.xy[0].is_finite() || q.xy[0].is_nan());
    let _ = v.unproject([0.0, 0.0]);
    let _ = v.project_path(&[], true);
}

#[test]
fn world_has_countries() {
    let w = world();
    assert!(w.len() > 150, "{}", w.len());
    let ua = country("UKR").unwrap();
    assert_eq!(ua.name, "Ukraine");
    assert!(country("france").is_some());
    let b = ua.geometry.bounds().unwrap();
    assert!(b[0] > 20.0 && b[2] < 41.0 && b[1] > 44.0 && b[3] < 53.0, "{b:?}");
}

#[test]
fn geojson_is_lenient() {
    let p = geojson::parse(
        r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{"name":"A"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}},
        {"type":"Feature","properties":{"name":"B"},"geometry":{"type":"LineString","coordinates":[[0,0],["x",1],[2,2]]}},
        {"type":"Feature","properties":null,"geometry":null},
        {"type":"Feature","geometry":{"type":"Bogus"}}]}"#,
    )
    .unwrap();
    assert_eq!(p.features.len(), 4);
    assert_eq!(p.features[0].geometry.polygons[0][0].len(), 3);
    assert_eq!(p.features[1].geometry.lines[0].len(), 2);
    assert_eq!(p.skipped, 2);
    assert!(geojson::parse("[1,2]").is_err());
    assert!(geojson::parse("{").is_err());
}

#[test]
fn deeply_nested_collections_are_bounded() {
    let mut s = String::from(r#"{"type":"Point","coordinates":[1,2]}"#);
    for _ in 0..200 {
        s = format!(r#"{{"type":"GeometryCollection","geometries":[{s}]}}"#);
    }
    // serde_json's own recursion limit or ours: either way an error or a skip, never a crash.
    let _ = geojson::parse(&s);
}

#[test]
fn fly_to_ends_on_target_and_zooms_out_between() {
    let a = fly::Stop { center: [2.35, 48.85], zoom: 6.0 }; // Paris
    let b = fly::Stop { center: [139.7, 35.7], zoom: 6.0 }; // Tokyo
    let path = fly::Path::new(a, b);
    let s0 = path.at(0.0);
    let s1 = path.at(1.0);
    assert!((s0.center[0] - a.center[0]).abs() < 1e-9 && (s0.zoom - 6.0).abs() < 1e-9);
    assert!((s1.center[0] - b.center[0]).abs() < 1e-6 && (s1.center[1] - b.center[1]).abs() < 1e-6, "{s1:?}");
    assert!((s1.zoom - 6.0).abs() < 1e-6);
    let mid = path.at(0.5);
    assert!(mid.zoom < 3.0, "the camera rises mid-flight: {mid:?}");
    assert!(path.length > 0.0);
}

#[test]
fn fly_to_pure_zoom_and_short_way_round() {
    let p = fly::Path::new(fly::Stop { center: [30.0, 50.0], zoom: 1.0 }, fly::Stop { center: [30.0, 50.0], zoom: 5.0 });
    assert!((p.at(0.5).zoom - 3.0).abs() < 1e-9);
    // 170°E to 170°W crosses the antimeridian (20°), not the long way (340°).
    let q = fly::Path::new(fly::Stop { center: [170.0, 0.0], zoom: 4.0 }, fly::Stop { center: [-170.0, 0.0], zoom: 4.0 });
    let m = q.at(0.5).center[0];
    assert!(m.abs() > 170.0, "{m}");
}

#[test]
fn slerp_follows_the_great_circle() {
    let m = slerp([0.0, 0.0], [90.0, 0.0], 0.5);
    assert!((m[0] - 45.0).abs() < 1e-9 && m[1].abs() < 1e-9);
    assert!((great_circle_deg([0.0, 0.0], [0.0, 90.0]) - 90.0).abs() < 1e-9);
}

#[test]
fn detail_switches_with_zoom_and_50m_has_more_countries() {
    let mut v = View::new([10.0, 50.0], 0.0, Projection::Equirectangular, 1920.0, 1080.0);
    assert_eq!(v.detail(), Detail::Low);
    v.zoom = 3.0;
    assert_eq!(v.detail(), Detail::Medium);
    assert!(world_at(Detail::Medium).len() > world_at(Detail::Low).len());
    // Small countries only the 1:50m set has.
    assert!(country("Singapore").is_some());
    assert!(country("FRA").is_some());
}

#[test]
fn may_show_culls_far_away_rings_only() {
    let v = View::new([10.0, 50.0], 4.0, Projection::Mercator, 1920.0, 1080.0);
    assert!(v.may_show([5.0, 45.0, 15.0, 55.0]));
    assert!(!v.may_show([130.0, -40.0, 150.0, -20.0]), "Australia is off a Europe frame");
    // Rotated and globe views never cull; nor do hostile bounds.
    let mut r = v;
    r.rotation = 30.0;
    assert!(r.may_show([5.0, 45.0, 15.0, 55.0]));
    let g = View::new([10.0, 50.0], 4.0, Projection::Orthographic, 1920.0, 1080.0);
    assert!(g.may_show([130.0, -40.0, 150.0, -20.0]));
    assert!(v.may_show([f64::NAN, 0.0, 1.0, 1.0]));
}

#[test]
fn lookup_cities_countries_and_coordinates() {
    let near = |h: Option<Hit>, lon: f64, lat: f64| {
        let p = h.map(|h| h.at()).unwrap_or([999.0, 999.0]);
        assert!((p[0] - lon).abs() < 0.5 && (p[1] - lat).abs() < 0.5, "{p:?} vs {lon},{lat}");
    };
    near(lookup("Kyiv"), 30.5, 50.45);
    near(lookup("paris"), 2.35, 48.86);
    near(lookup("Valencia"), -67.98, 10.23); // the bigger one, in Venezuela
    near(lookup("Valencia, Spain"), -0.40, 39.49);
    near(lookup("Portland, Maine"), -70.25, 43.67);
    near(lookup("cordoba, ESP"), -4.77, 37.88);
    near(lookup("48.8566, 2.3522"), 2.3522, 48.8566);
    near(lookup("2.35E 48.85N"), 2.35, 48.85);
    near(lookup("33.9S, 151.2E"), 151.2, -33.9);
    // France's anchor is in Europe, not French Guiana.
    near(lookup("FRA"), 2.5, 46.6);
    assert!(lookup("Atlantis").is_none());
    assert!(lookup("").is_none());
    assert!(lookup("91, 0").is_none());
    assert!(lookup("NaN, 3").is_none());
    let b = lookup("France").and_then(|h| h.bounds()).unwrap_or_default();
    assert!(b[0] > -10.0, "main landmass only: {b:?}");
}

#[test]
fn search_suggests_prefixes_first() {
    let hits = places::search("ber", 5);
    assert!(!hits.is_empty() && hits.len() <= 5);
    assert!(hits[0].name().to_lowercase().starts_with("ber"));
    assert!(places::search("", 5).is_empty());
    assert!(places::parse_tsv("bad\nrow\twith\tfew\tcols\n\t\t\t\t\t1\t2\t3\t0\n").is_empty());
}
