//! The colours of the interface.
//!
//! The palette lives in one place, so the whole interface reads as one
//! machine rather than a collection of unrelated panels. Every colour is a
//! 256-colour index: no terminal can be relied on to show more, and the
//! greyscale part of the ramp degrades perfectly on a terminal with only
//! sixteen colours.

use ratatui::style::Color;

use crate::config::ThemeKind;

/// The colours everything is drawn in.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// The background behind everything.
    pub void: Color,
    /// The interior of a panel.
    pub panel: Color,
    /// The unfilled part of a meter, and the hairline rules.
    pub grid: Color,
    /// The border of a panel that is not the one in use.
    pub frame: Color,
    /// Secondary text: ids, tool output, the thinking.
    pub muted: Color,
    /// Body text.
    pub text: Color,
    /// The background of the selected row.
    pub sel: Color,
    /// The accent: the prompt, the active border, the header.
    pub accent: Color,
    /// The model name and the headings.
    pub heading: Color,
    /// The assistant's own words.
    pub assistant: Color,
    /// The words of the person.
    pub user: Color,
    /// The names of the tools.
    pub tool: Color,
    /// What is being done, and what needs attention.
    pub warn: Color,
    /// What is finished, and what worked.
    pub ok: Color,
    /// What went wrong.
    pub err: Color,
    /// A note that is neither good nor bad.
    pub info: Color,
}

impl Theme {
    /// The palette for a dark background.
    pub fn dark() -> Theme {
        Theme {
            void: Color::Indexed(232),
            panel: Color::Indexed(234),
            grid: Color::Indexed(236),
            frame: Color::Indexed(238),
            muted: Color::Indexed(243),
            text: Color::Indexed(252),
            sel: Color::Indexed(237),
            accent: Color::Indexed(39),
            heading: Color::Indexed(81),
            assistant: Color::Indexed(255),
            user: Color::Indexed(222),
            tool: Color::Indexed(141),
            warn: Color::Indexed(214),
            ok: Color::Indexed(114),
            err: Color::Indexed(203),
            info: Color::Indexed(75),
        }
    }

    /// The palette for a light background.
    pub fn light() -> Theme {
        Theme {
            void: Color::Indexed(255),
            panel: Color::Indexed(254),
            grid: Color::Indexed(251),
            frame: Color::Indexed(248),
            muted: Color::Indexed(241),
            text: Color::Indexed(234),
            sel: Color::Indexed(252),
            accent: Color::Indexed(25),
            heading: Color::Indexed(24),
            assistant: Color::Indexed(233),
            user: Color::Indexed(130),
            tool: Color::Indexed(90),
            warn: Color::Indexed(130),
            ok: Color::Indexed(28),
            err: Color::Indexed(160),
            info: Color::Indexed(26),
        }
    }

    /// The palette for a choice the user made.
    pub fn for_kind(kind: ThemeKind) -> Theme {
        match kind {
            ThemeKind::Dark => Theme::dark(),
            ThemeKind::Light => Theme::light(),
            ThemeKind::Auto => Theme::detect(),
        }
    }

    /// Work out which palette to use.
    ///
    /// `COLORFGBG` is the closest thing to a portable way of asking: many
    /// terminals set it to "foreground;background", where a background of
    /// 0..6 or 8 is dark. When it says nothing, the dark palette is used,
    /// because a terminal interface is usually read on a dark background.
    pub fn detect() -> Theme {
        match std::env::var("COLORFGBG") {
            Ok(value) => {
                let background = value
                    .rsplit(';')
                    .next()
                    .and_then(|part| part.trim().parse::<u8>().ok());
                match background {
                    Some(index) if index >= 8 => Theme::light(),
                    _ => Theme::dark(),
                }
            }
            Err(_) => Theme::dark(),
        }
    }
}
