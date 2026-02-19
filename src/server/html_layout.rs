use axum::response::Html;
use maud::{DOCTYPE, Markup, PreEscaped, html};

const POINTER_LOGO_SVG: &str = include_str!("assets/pointer.svg");

pub(crate) fn page(title: &str, content: Markup) -> Html<String> {
    Html(
        html! {
            (DOCTYPE)
            html lang="en" {
                head {
                    meta charset="utf-8";
                    meta name="viewport" content="width=device-width, initial-scale=1";
                    title { (title) }
                    link rel="stylesheet" href="https://use.typekit.net/bpw6fxi.css";
                    link rel="stylesheet" href="/assets/reset.css";
                    link rel="stylesheet" href="/assets/app.css";
                    script type="module" src="https://cdn.jsdelivr.net/npm/@github/relative-time-element/+esm" {}
                    script src="/assets/app.js" defer {}
                }
                body {
                    header class="stack hstack align-center" {
                        h1 class="hstack align-center" { a href="/hyperlinks" {
                            span { "Hyperlinks" }
                        } }

                        span class="logo" aria-hidden="true" { (PreEscaped(POINTER_LOGO_SVG)) }
                        span class="spacer" aria-hidden="true" {}

                        nav { a href="/hyperlinks/new" { "New hyperlink" } }
                    }
                    main { (content) }
                }
            }
        }
        .into_string(),
    )
}
