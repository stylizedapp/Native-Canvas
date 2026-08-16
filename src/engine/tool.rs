/// Активный инструмент.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tool {
    Select,
    Pan,
    Rectangle,
    Ellipse,
    Line,
    Frame,
}

impl Tool {
    pub fn from_name(name: &str) -> Self {
        match name {
            "pan" => Tool::Pan,
            "rectangle" => Tool::Rectangle,
            "ellipse" => Tool::Ellipse,
            "line" => Tool::Line,
            "frame" => Tool::Frame,
            _ => Tool::Select,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Pan => "pan",
            Tool::Rectangle => "rectangle",
            Tool::Ellipse => "ellipse",
            Tool::Line => "line",
            Tool::Frame => "frame",
        }
    }
}