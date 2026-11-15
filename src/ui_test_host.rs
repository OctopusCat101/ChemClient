//! Test adapter for the API 4 borrowed-builder/owned-clone contract.
pub use crate::astrobox::psys_host_v4::ui::{ElementType, Event, FlexDirection};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Debug, serde::Serialize)]
pub struct Node {
    pub id: u64,
    pub kind: String,
    pub content: Option<String>,
    pub props: BTreeMap<String, String>,
    pub styles: BTreeMap<String, String>,
    pub events: Vec<(String, String)>,
    pub children: Vec<Node>,
}
#[derive(Clone)]
pub struct Element(RefCell<Node>);
static NEXT: AtomicU64 = AtomicU64::new(1);
thread_local! { pub static RENDERS: RefCell<Vec<(String, Node)>> = const { RefCell::new(Vec::new()) }; }
macro_rules! number_styles {
    ($($name:ident => $css:literal),* $(,)?) => {$(
        pub fn $name(&self, value: u32) -> Self { self.style($css, format!("{value}px")) }
    )*};
}
impl Element {
    pub fn new(kind: ElementType, content: Option<&str>) -> Self {
        Self(RefCell::new(Node {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            kind: format!("{kind:?}"),
            content: content.map(str::to_owned),
            props: Default::default(),
            styles: Default::default(),
            events: Vec::new(),
            children: Vec::new(),
        }))
    }
    pub fn snapshot(&self) -> Node {
        self.0.borrow().clone()
    }
    fn edit(&self, f: impl FnOnce(&mut Node)) -> Self {
        f(&mut self.0.borrow_mut());
        self.clone()
    }
    fn style(&self, name: &str, value: impl Into<String>) -> Self {
        self.edit(|n| {
            n.styles.insert(name.into(), value.into());
        })
    }
    pub fn content(&self, content: Option<&str>) -> Self {
        self.edit(|n| n.content = content.map(str::to_owned))
    }
    pub fn prop(&self, name: &str, value: &str) -> Self {
        self.edit(|n| {
            n.props.insert(name.into(), value.into());
        })
    }
    pub fn child(&self, child: Self) -> Self {
        self.edit(|n| n.children.push(child.snapshot()))
    }
    pub fn on(&self, event: Event, id: &str) -> Self {
        self.edit(|n| n.events.push((format!("{event:?}"), id.into())))
    }
    pub fn without_default_styles(&self) -> Self {
        self.clone()
    }
    pub fn flex(&self) -> Self {
        self.style("display", "flex")
    }
    pub fn flex_direction(&self, dir: FlexDirection) -> Self {
        self.style(
            "flex-direction",
            match dir {
                FlexDirection::Row => "row",
                FlexDirection::Column => "column",
                FlexDirection::RowReverse => "row-reverse",
                FlexDirection::ColumnReverse => "column-reverse",
            },
        )
    }
    pub fn align_center(&self) -> Self {
        self.style("align-items", "center")
    }
    pub fn justify_center(&self) -> Self {
        self.style("justify-content", "center")
    }
    pub fn width_full(&self) -> Self {
        self.style("width", "100%")
    }
    pub fn relative(&self) -> Self {
        self.style("position", "relative")
    }
    pub fn absolute(&self) -> Self {
        self.style("position", "absolute")
    }
    pub fn disabled(&self) -> Self {
        self.prop("disabled", "true")
    }
    pub fn flex_grow(&self, value: f32) -> Self {
        self.style("flex-grow", value.to_string())
    }
    pub fn flex_shrink(&self, value: f32) -> Self {
        self.style("flex-shrink", value.to_string())
    }
    pub fn opacity(&self, value: f32) -> Self {
        self.style("opacity", value.to_string())
    }
    pub fn bg(&self, value: &str) -> Self {
        self.style("background", value)
    }
    pub fn text_color(&self, value: &str) -> Self {
        self.style("color", value)
    }
    pub fn animation(&self, value: &str) -> Self {
        self.style("animation", value)
    }
    pub fn transition(&self, value: &str) -> Self {
        self.style("transition", value)
    }
    pub fn transform(&self, value: &str) -> Self {
        self.style("transform", value)
    }
    pub fn border(&self, width: u32, color: &str) -> Self {
        self.style("border", format!("{width}px solid {color}"))
    }
    number_styles! {
        width => "width", height => "height", min_width => "min-width", max_width => "max-width",
        min_height => "min-height", max_height => "max-height", margin => "margin", padding => "padding",
        padding_top => "padding-top", padding_bottom => "padding-bottom", padding_left => "padding-left",
        padding_right => "padding-right", gap => "gap", radius => "border-radius", size => "font-size",
        top => "top", left => "left",
    }
}
pub fn render(root: &str, el: Element) {
    RENDERS.with(|r| r.borrow_mut().push((root.into(), el.snapshot())));
}
