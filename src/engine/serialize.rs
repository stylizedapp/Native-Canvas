use crate::engine::model::document::Document;
use serde::{Deserialize, Serialize};

/// Внешний (JSON) конверт документа. Внутри — полная модель (`Document`),
/// сериализуемая напрямую (все типы модели имеют derive(Serialize/Deserialize)).
/// Обёртка добавляет версионирование формата.
#[derive(Serialize, Deserialize)]
struct FileDoc {
    app: String,
    version: u32,
    document: Document,
}

/// Текущая версия формата файла.
const FORMAT_VERSION: u32 = 2;

pub fn save_json(doc: &Document) -> Result<String, serde_json::Error> {
    let wrapper = FileDoc {
        app: "native_canvas".into(),
        version: FORMAT_VERSION,
        document: doc.clone(),
    };
    serde_json::to_string_pretty(&wrapper)
}

pub fn load_json(data: &str) -> Result<Document, Box<dyn std::error::Error>> {
    let wrapper: FileDoc = serde_json::from_str(data)?;
    if wrapper.version != FORMAT_VERSION {
        return Err(format!("Неподдерживаемая версия документа: {}", wrapper.version).into());
    }
    let mut doc = wrapper.document;
    // Актуальные корни графа сцены — источник истины для списка верхних узлов страницы.
    let roots = doc.scene.roots().to_vec();
    if let Some(page) = doc.active_page_mut() {
        page.top_level = roots;
    }
    Ok(doc)
}