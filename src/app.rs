pub mod api;
mod components;
mod events;
mod pages;

use dioxus::prelude::*;

pub use pages::Home;

static STYLES: Asset = asset!("/assets/tailwind.css");

#[component]
pub fn App() -> Element {
    rsx! {
      document::Meta {
        name: "viewport",
        content: "width=device-width, initial-scale=1.0",
      }
      document::Link {
        rel: "icon",
        href: "data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 viewBox=%220 0 100 100%22><text y=%22.9em%22 font-size=%2290%22>◈</text></svg>",
      }
      document::Stylesheet { href: STYLES }
      document::Title { "Tessora · M3U8 下载管理" }
      Home {}
    }
}
