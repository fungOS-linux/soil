use soil_scene::{Content, Image, Ink, Node, Present, Rect, RgbaFrame, Scene, Text, Transform};

fn rect(id: &str) -> Node {
    Node {
        transition_ms: None,
        id: id.into(),
        parent: None,
        transform: Transform::default(),
        content: Content::Rect(Rect::default()),
    }
}

fn text(id: &str, body: &str) -> Node {
    Node {
        transition_ms: None,
        id: id.into(),
        parent: None,
        transform: Transform::default(),
        content: Content::Text(Text {
            text: body.into(),
            ink: Some(Ink::Text),
            rgb: None,
            opacity: 1.0,
        }),
    }
}

#[test]
fn a_scene_round_trips_through_the_wire() {
    let present = Present::Scene(Scene::new(vec![rect("ground"), text("title", "Hello")]));
    let bytes = present.to_bytes().expect("encode");
    let back = Present::from_bytes(&bytes).expect("decode");
    assert_eq!(back, present);
}

#[test]
fn the_host_refuses_a_schema_it_does_not_draw() {
    let mut scene = Scene::new(vec![rect("a")]);
    scene.schema = 99;
    let error = scene.validate().expect_err("a newer schema is refused");
    assert!(error.0.contains("schema"), "{error}");
}

#[test]
fn duplicate_ids_are_refused() {
    let error = Scene::new(vec![rect("a"), rect("a")])
        .validate()
        .expect_err("two nodes cannot share an id");
    assert!(error.0.contains("duplicate"), "{error}");
}

#[test]
fn a_parent_must_precede_its_child() {
    let mut child = rect("child");
    child.parent = Some("parent".into());
    let error = Scene::new(vec![child, rect("parent")])
        .validate()
        .expect_err("a forward reference draws differently per parse order");
    assert!(error.0.contains("parent"), "{error}");

    let mut child = rect("child");
    child.parent = Some("parent".into());
    Scene::new(vec![rect("parent"), child])
        .validate()
        .expect("a parent before its child is the rule");
}

#[test]
fn text_is_bounded_and_printable() {
    let long = "x".repeat(soil_scene::MAX_TEXT_BYTES + 1);
    assert!(Scene::new(vec![text("a", &long)]).validate().is_err());
    assert!(
        Scene::new(vec![text("a", "bell\u{7}")]).validate().is_err(),
        "a control character is not text"
    );
    Scene::new(vec![text("a", "two\nlines\tand a tab")])
        .validate()
        .expect("newlines and tabs are text");
}

#[test]
fn an_asset_is_an_id_never_a_path() {
    let node = Node {
        id: "image".into(),
        parent: None,
        transition_ms: None,
        transform: Transform::default(),
        content: Content::Image(Image {
            asset: "../../etc/passwd".into(),
        }),
    };
    let error = Scene::new(vec![node])
        .validate()
        .expect_err("a document must not steer the host at a file");
    assert!(error.0.contains("safe identifier"), "{error}");
}

#[test]
fn an_rgba_frame_must_match_its_extent() {
    use soil_scene::serde_bytes_lite::Bytes;
    let good = Present::Rgba(RgbaFrame {
        width: 2,
        height: 1,
        pixels: Bytes(vec![0; 8]),
    });
    good.validate().expect("2x1 RGBA8 is eight bytes");

    let short = Present::Rgba(RgbaFrame {
        width: 2,
        height: 1,
        pixels: Bytes(vec![0; 7]),
    });
    let error = short
        .validate()
        .expect_err("a short buffer is a torn frame");
    assert!(error.0.contains("bytes"), "{error}");
}

#[test]
fn a_gpu_lease_is_an_id_and_an_extent() {
    let lease = Present::GpuLease(soil_scene::GpuLease {
        source_id: "model-3".into(),
        width: 640,
        height: 480,
        revision: 7,
    });
    lease.validate().expect("a lease names a source");
    let bytes = lease.to_bytes().expect("encode");
    assert_eq!(Present::from_bytes(&bytes).unwrap(), lease);
}

#[test]
fn opacity_and_transforms_must_be_finite() {
    let mut node = rect("a");
    if let Content::Rect(rect) = &mut node.content {
        rect.opacity = f32::NAN;
    }
    assert!(Scene::new(vec![node]).validate().is_err());

    let mut node = rect("b");
    node.transform.scale = [1.0, f32::INFINITY, 1.0];
    assert!(Scene::new(vec![node]).validate().is_err());
}

#[test]
fn a_transform_defaults_to_identity() {
    let node: Node = serde_json::from_str(r#"{"id":"a","type":"group"}"#).expect("minimal node");
    assert_eq!(node.transform.scale, [1.0, 1.0, 1.0]);
    assert_eq!(node.transform.translation, [0.0, 0.0, 0.0]);
}

#[test]
fn a_polyline_is_a_bounded_scene_primitive() {
    let scene = Scene::new(vec![Node {
        id: "line".into(),
        parent: None,
        transition_ms: None,
        transform: Transform::default(),
        content: Content::Polyline(soil_scene::Polyline {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            ink: Some(Ink::Primary),
            rgb: None,
            rgb2: None,
            width: 0.1,
            opacity: 1.0,
            join: soil_scene::LineJoin::Miter,
            miter_limit: 4.0,
        }),
    }]);
    scene.validate().expect("valid polyline");
}

#[test]
fn an_area_is_a_bounded_scene_primitive() {
    let scene = Scene::new(vec![Node {
        id: "area".into(),
        parent: None,
        transition_ms: None,
        transform: Transform::default(),
        content: Content::Area(soil_scene::Area {
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            baseline: -1.0,
            ink: Some(Ink::Primary),
            rgb: None,
            opacity: 0.4,
            base_opacity: None,
        }),
    }]);
    scene.validate().expect("valid area");
}

#[test]
fn polyline_joins_are_validated() {
    let base = soil_scene::Polyline {
        points: vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]],
        ink: Some(Ink::Primary),
        rgb: None,
        rgb2: None,
        width: 0.1,
        opacity: 1.0,
        join: soil_scene::LineJoin::Round,
        miter_limit: 4.0,
    };
    let node = |content| Node {
        id: "line".into(),
        parent: None,
        transition_ms: None,
        transform: Transform::default(),
        content,
    };
    Scene::new(vec![node(Content::Polyline(base.clone()))])
        .validate()
        .expect("round join");
    let mut invalid = base;
    invalid.miter_limit = 0.0;
    assert!(
        Scene::new(vec![node(Content::Polyline(invalid))])
            .validate()
            .is_err()
    );
}

#[test]
fn chart_catalog_primitives_are_bounded() {
    let node = |id: &str, content| Node {
        id: id.into(),
        parent: None,
        transition_ms: None,
        transform: Transform::default(),
        content,
    };
    Scene::new(vec![
        node(
            "points",
            Content::Points(soil_scene::Points {
                points: vec![[0.0, 0.0], [0.5, 0.5]],
                size: [0.02, 0.02],
                shape: soil_scene::PointShape::Circle,
                ink: Some(Ink::Info),
                rgb: None,
                opacity: 0.8,
            }),
        ),
        node(
            "heatmap",
            Content::Heatmap(soil_scene::Heatmap {
                rows: 2,
                cols: 2,
                values: vec![0.0, 1.0, 2.0, 3.0],
                min: None,
                max: None,
                opacity: 0.9,
                zero_transparent: false,
            }),
        ),
        node(
            "candles",
            Content::Candles(soil_scene::Candles {
                candles: vec![soil_scene::Candle {
                    x: 0.0,
                    open: 1.0,
                    high: 2.0,
                    low: 0.0,
                    close: 1.5,
                }],
                width: 0.1,
                up_ink: Some(Ink::Success),
                down_ink: Some(Ink::Danger),
                up_rgb: None,
                down_rgb: None,
                opacity: 1.0,
            }),
        ),
        node(
            "band",
            Content::Band(soil_scene::Band {
                upper: vec![[0.0, 1.0], [1.0, 1.5]],
                lower: vec![[0.0, 0.0], [1.0, 0.5]],
                ink: Some(Ink::Warning),
                rgb: None,
                opacity: 0.3,
            }),
        ),
    ])
    .validate()
    .expect("valid chart catalog");

    assert!(
        Scene::new(vec![node(
            "bad-heatmap",
            Content::Heatmap(soil_scene::Heatmap {
                rows: 2,
                cols: 2,
                values: vec![0.0],
                min: None,
                max: None,
                opacity: 1.0,
                zero_transparent: false,
            }),
        )])
        .validate()
        .is_err()
    );

    assert!(
        Scene::new(vec![node(
            "bad-band",
            Content::Band(soil_scene::Band {
                upper: vec![[0.0, 1.0], [1.0, 1.5]],
                lower: vec![[0.0, 0.0]],
                ink: None,
                rgb: None,
                opacity: 1.0,
            }),
        )])
        .validate()
        .is_err()
    );
}
