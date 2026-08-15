use super::scene::{Fill, NodeKind, Scene, SceneNode, Stroke};
use serde::{Deserialize, Serialize};

/// Внешнее (JSON) представление узла. Формат намеренно плоский и версионируемый,
/// чтобы сохранять совместимость при развитии модели данных.
#[derive(Serialize, Deserialize, Clone)]
struct FileNode {
    id: u64,
    name: String,
    parent: Option<u64>,
    kind: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    x2: f32,
    y2: f32,
    fill: Option<[u8; 4]>,
    stroke_color: Option<[u8; 4]>,
    stroke_width: f32,
    opacity: f32,
    visible: bool,
}

#[derive(Serialize, Deserialize)]
struct FileDoc {
    app: String,
    version: u32,
    roots: Vec<u64>,
    nodes: Vec<FileNode>,
}

pub fn save_json(scene: &Scene) -> Result<String, serde_json::Error> {
    let mut nodes: Vec<FileNode> = Vec::new();
    for (_, n) in scene.nodes.iter() {
        let (w, h, x2, y2) = match n.kind {
            NodeKind::Frame { w, h } | NodeKind::Rectangle { w, h } | NodeKind::Ellipse { w, h } => {
                (w, h, 0.0, 0.0)
            }
            NodeKind::Line { x2, y2 } => (0.0, 0.0, x2, y2),
            _ => (0.0, 0.0, 0.0, 0.0),
        };
        nodes.push(FileNode {
            id: n.id,
            name: n.name.clone(),
            parent: n.parent,
            kind: kind_name(&n.kind),
            x: n.transform.translation.x,
            y: n.transform.translation.y,
            w,
            h,
            x2,
            y2,
            fill: n.fill.map(|f| f.color),
            stroke_color: n.stroke.map(|s| s.color),
            stroke_width: n.stroke.map(|s| s.width).unwrap_or(0.0),
            opacity: n.opacity,
            visible: n.visible,
        });
    }

    let doc = FileDoc {
        app: "native_canvas".into(),
        version: 1,
        roots: scene.roots.iter().copied().collect(),
        nodes,
    };
    serde_json::to_string_pretty(&doc)
}

pub fn load_json(data: &str) -> Result<Scene, Box<dyn std::error::Error>> {
    let doc: FileDoc = serde_json::from_str(data)?;

    let mut scene = Scene::new();
    let mut remaining: Vec<&FileNode> = doc.nodes.iter().collect();

    // Итеративно вставляем узлы: корни (parent == None) — сразу, дети — когда
    // родитель уже вставлен. Обработка идёт послойно, пока всё не размещено.
    while !remaining.is_empty() {
        let mut progress = false;
        let mut still: Vec<&FileNode> = Vec::new();
        for f in remaining {
            let parent_ok = match f.parent {
                None => true,
                Some(p) => scene.get(p).is_some(),
            };
            if parent_ok {
                let node = build_node(f);
                let id = f.id;
                match f.parent {
                    Some(p) => scene.add_child(p, node),
                    None => scene.add_root(node),
                };
                let _ = id;
                progress = true;
            } else {
                still.push(f);
            }
        }
        if !progress {
            // Обнаружен цикл или висячий родитель.
            return Err("Некорректная структура документа (родитель не найден)".into());
        }
        remaining = still;
    }

    // Гарантируем, что счётчик id превосходит все загруженные.
    let max_id = doc.nodes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
    scene.ensure_next_id(max_id);

    Ok(scene)
}

fn build_node(f: &FileNode) -> SceneNode {
    let mut node = SceneNode::new(f.id, &f.name, kind_from_name(&f.kind, f));
    node.transform = glam::Affine2::from_translation(glam::Vec2::new(f.x, f.y));
    node.fill = f.fill.map(|c| Fill { color: c });
    node.stroke = if f.stroke_width > 0.0 {
        Some(Stroke {
            color: f.stroke_color.unwrap_or([0, 0, 0, 255]),
            width: f.stroke_width,
            inside: false,
            center: true,
            outside: false,
        })
    } else {
        None
    };
    node.opacity = f.opacity;
    node.visible = f.visible;
    node
}

fn kind_name(kind: &NodeKind) -> String {
    match kind {
        NodeKind::Frame { .. } => "frame".into(),
        NodeKind::Group => "group".into(),
        NodeKind::Rectangle { .. } => "rectangle".into(),
        NodeKind::Ellipse { .. } => "ellipse".into(),
        NodeKind::Line { .. } => "line".into(),
        NodeKind::Vector => "vector".into(),
    }
}

fn kind_from_name(name: &str, f: &FileNode) -> NodeKind {
    match name {
        "frame" => NodeKind::Frame { w: f.w, h: f.h },
        "group" => NodeKind::Group,
        "rectangle" => NodeKind::Rectangle { w: f.w, h: f.h },
        "ellipse" => NodeKind::Ellipse { w: f.w, h: f.h },
        "line" => NodeKind::Line { x2: f.x2, y2: f.y2 },
        _ => NodeKind::Rectangle { w: f.w, h: f.h },
    }
}