use gpui::{AssetSource, Result, SharedString, Styled, Svg, px, rgb, svg};
use std::borrow::Cow;

/// The native shell's glyphs, embedded locally.
pub struct IconAssets;

macro_rules! glyph {
    ($path:literal) => {
        concat!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d=""#,
            $path,
            r#""/></svg>"#
        )
    };
}

impl AssetSource for IconAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let data = match path {
            "kadr/home.svg" => glyph!("m3 10 9-7 9 7M5 9v11h14V9M9 20v-6h6v6"),
            "kadr/film.svg" => glyph!(
                "M5 4h14a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2ZM7 4v16m10-16v16M3 9h4m10 0h4M3 15h4m10 0h4"
            ),
            "kadr/tv.svg" => glyph!(
                "M5 6h14a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2Zm3-4 4 4 4-4"
            ),
            "kadr/bookmark.svg" => glyph!("M6 3h12v18l-6-4-6 4Z"),
            "kadr/clock.svg" => glyph!("M21 12a9 9 0 1 1-18 0 9 9 0 1 1 18 0Zm-9-5v5l3 2"),
            "kadr/download.svg" => glyph!("M12 3v12m-4-4 4 4 4-4M5 20h14"),
            "kadr/settings.svg" => glyph!("M4 6h16M4 12h16M4 18h16M8 4v4m8 2v4m-6 2v4"),
            "kadr/sliders.svg" => {
                glyph!("M4 6h9m4 0h3M4 12h3m4 0h9M4 18h7m4 0h5m-7-14v4m-6 2v4m4 2v4")
            }
            "kadr/search.svg" => glyph!("M18 11a7 7 0 1 1-14 0 7 7 0 1 1 14 0Zm-2 5 4 4"),
            "kadr/play.svg" => glyph!("m8 5 11 7-11 7Z"),
            "kadr/plus.svg" => glyph!("M12 5v14M5 12h14"),
            "kadr/check.svg" => glyph!("m5 12 4 4L19 6"),
            "kadr/chevron.svg" => glyph!("m9 18 6-6-6-6"),
            "kadr/back.svg" => glyph!("m15 18-6-6 6-6"),
            "kadr/close.svg" => glyph!("m6 6 12 12M18 6 6 18"),
            "kadr/link.svg" => glyph!(
                "M10 13a4 4 0 0 0 6 .3l3-3a4 4 0 0 0-5.7-5.7L11.6 6m2.4 5a4 4 0 0 0-6-.3l-3 3a4 4 0 0 0 5.7 5.7l1.7-1.4"
            ),
            _ => return Ok(None),
        };
        Ok(Some(Cow::Borrowed(data.as_bytes())))
    }

    fn list(&self, _: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

pub(super) fn icon(name: &str, size: f32, color: u32) -> Svg {
    svg()
        .path(format!("kadr/{name}.svg"))
        .size(px(size))
        .flex_shrink_0()
        .text_color(rgb(color))
}
