//! Each tool's mark. Claude's and Codex's are the ones Runway shows, Codex's traced from the bitmap
//! it ships. Pi's is the pixel grid in Pi's own `pi-logo`. Hyper's is its app icon's two panes.

use rune::color::hex;
use rune::{Color, El, Icon, icon, row};
use rune_kit::Kit;

use crate::config::Owner;

static CLAUDE: Icon = Icon {
    name: "sb-claude",
    view: 12.0,
    body: r#"<path d="M2.355 7.977L4.715 6.652L4.754 6.539L4.715 6.477L4.602 6.477L4.207 6.453L2.859 6.418L1.691 6.371L0.559 6.309L0.273 6.25L0 5.891L0.027 5.715L0.266 5.555L0.609 5.586L1.371 5.637L2.512 5.715L3.336 5.762L4.559 5.891L4.754 5.891L4.781 5.812L4.715 5.762L4.664 5.715L3.484 4.918L2.207 4.074L1.539 3.59L1.176 3.344L0.992 3.113L0.914 2.609L1.242 2.25L1.684 2.281L1.797 2.312L2.242 2.656L3.195 3.395L4.441 4.312L4.625 4.465L4.699 4.414L4.707 4.379L4.625 4.242L3.949 3.02L3.227 1.773L2.906 1.258L2.82 0.949L2.82 0.938C2.789 0.82 2.77 0.695 2.766 0.574L3.141 0.066L3.348 0L3.844 0.066L4.055 0.25L4.363 0.957L4.863 2.07L5.641 3.586L5.867 4.035L5.988 4.449L6.035 4.578L6.113 4.578L6.117 4.504L6.18 3.652L6.297 2.605L6.41 1.258L6.449 0.879L6.637 0.426L7.012 0.18L7.305 0.32L7.543 0.664L7.508 0.887L7.363 1.812L7.082 3.266L6.898 4.238L7.004 4.238L7.125 4.117L7.617 3.465L8.441 2.434L8.805 2.023L9.23 1.57L9.504 1.355L10.02 1.355L10.398 1.922L10.227 2.504L9.695 3.176L9.254 3.746L8.621 4.598L8.227 5.277L8.262 5.332L8.355 5.32L9.785 5.016L10.559 4.875L11.48 4.719L11.898 4.914L11.945 5.113L11.781 5.516L10.797 5.758L9.641 5.988L7.922 6.395L7.902 6.41L7.926 6.441L8.699 6.516L9.031 6.535L9.844 6.535L11.355 6.648L11.75 6.91L11.988 7.23L11.949 7.473L11.34 7.781L10.52 7.586L8.605 7.133L7.949 6.969L7.859 6.969L7.859 7.023L8.406 7.559L9.41 8.465L10.664 9.629L10.727 9.918L10.566 10.145L10.395 10.121L9.293 9.293L8.867 8.918L7.902 8.109L7.84 8.109L7.84 8.195L8.062 8.52L9.234 10.281L9.297 10.82L9.211 10.996L8.906 11.102L8.57 11.039L7.883 10.078L7.176 8.996L6.605 8.023L6.535 8.062L6.199 11.691L6.043 11.875L5.68 12.016L5.375 11.785L5.215 11.41L5.375 10.672L5.57 9.711L5.727 8.945L5.871 7.996L5.957 7.68L5.949 7.66L5.879 7.668L5.16 8.652L4.07 10.125L3.207 11.047L3 11.129L2.641 10.945L2.676 10.613L2.875 10.32L4.07 8.801L4.789 7.859L5.254 7.316L5.25 7.238L5.223 7.238L2.066 9.281L1.5 9.355L1.258 9.129L1.289 8.758L1.406 8.637L2.359 7.98L2.355 7.984Z" fill="currentColor"/>"#,
};

static CODEX: Icon = Icon {
    name: "sb-codex",
    view: 24.0,
    body: r#"<path d="M12.77 23.96C12.14 23.88 11.52 23.7 10.92 23.41C10.39 23.15 10.03 22.9 9.36 22.33L9.25 22.24L8.91 22.31C6.91 22.72 4.8 22.09 3.35 20.63C1.88 19.16 1.29 17.18 1.69 15.1L1.76 14.76L1.59 14.57C-0.65 12.15 -0.51 8.29 1.9 5.99C2.58 5.35 3.15 5 4.06 4.68L4.5 4.52L4.61 4.19C5.97 0.09 11.07 -1.34 14.37 1.46C14.78 1.8 14.75 1.79 14.88 1.75C15.32 1.62 16.18 1.55 16.75 1.59C20.46 1.89 22.99 5.22 22.31 8.91C22.23 9.34 22.2 9.24 22.57 9.68C25.34 12.84 23.91 17.98 19.89 19.36C19.51 19.49 19.45 19.53 19.42 19.65C19.41 19.7 19.34 19.9 19.26 20.09C18.23 22.74 15.57 24.32 12.77 23.96ZM6.89 16.16C6.96 16.13 7.07 16.05 7.12 16C7.22 15.89 9.18 12.62 9.27 12.41C9.33 12.27 9.33 11.98 9.27 11.83C9.17 11.59 7.24 8.25 7.15 8.16C6.63 7.63 5.71 8.01 5.72 8.75C5.72 8.97 5.75 9.02 6.65 10.61C7.4 11.9 7.5 12.11 7.48 12.17C7.46 12.21 7.06 12.88 6.59 13.67L5.74 15.11L5.72 15.34C5.69 15.99 6.29 16.41 6.89 16.16ZM17.98 16.15C18.61 15.85 18.61 14.95 17.99 14.64L17.83 14.56L15.2 14.56L12.58 14.56L12.42 14.64C11.87 14.89 11.78 15.62 12.23 16.02C12.46 16.22 12.35 16.21 15.2 16.22L17.83 16.22L17.98 16.15Z" fill="currentColor" fill-rule="evenodd"/>"#,
};

static PI_CORAL: Icon = Icon { name: "sb-pi-coral", view: 4.0, body: r#"<path d="M0 0H3V2H2V1H0Z" fill="currentColor"/>"# };
static PI_BLUE: Icon = Icon { name: "sb-pi-blue", view: 4.0, body: r#"<path d="M0 1H1V2H2V3H1V4H0Z" fill="currentColor"/>"# };
static PI_YELLOW: Icon = Icon { name: "sb-pi-yellow", view: 4.0, body: r#"<path d="M3 2H4V4H3Z" fill="currentColor"/>"# };

/// `assets/Hyper.icon`'s panes, moved to the middle of a square that fits them.
static HYPER: Icon = Icon {
    name: "sb-hyper",
    view: 560.0,
    body: r#"<rect x="6.6" y="68.75" width="248.5" height="422.5" rx="29.8" fill="currentColor"/><rect x="322.3" y="86.15" width="213.7" height="387.7" rx="12.4" fill="none" stroke="currentColor" stroke-width="34.8"/>"#,
};

/// Icons take one colour, so a mark is a stack of them. `None` follows the theme's ink.
pub type Layer = (&'static Icon, Option<&'static str>);

pub static CLAUDE_LAYERS: [Layer; 1] = [(&CLAUDE, Some("#D97757"))];
pub static CODEX_LAYERS: [Layer; 1] = [(&CODEX, None)];
pub static PI_LAYERS: [Layer; 3] = [(&PI_CORAL, Some("#E48A7A")), (&PI_BLUE, Some("#4F8EB3")), (&PI_YELLOW, Some("#EAB65D"))];
static HYPER_LAYERS: [Layer; 1] = [(&HYPER, None)];

fn layers(owner: Owner) -> &'static [Layer] {
    match owner {
        Owner::Tool(source) => source.agent().logo(),
        Owner::Hyper => &HYPER_LAYERS,
    }
}

/// `owner`'s mark, `size` points square. Hidden from assistive tech: what holds it names the tool.
pub fn logo(owner: impl Into<Owner>, size: f32, k: &Kit) -> El {
    let owner = owner.into();
    let ink = |color: Option<&str>| -> Color { color.and_then(hex).unwrap_or(k.c.icon_base) };
    let marks = layers(owner).iter().map(|(mark, color)| icon(mark, size).key(mark.name).color(ink(*color)).absolute(0.0, 0.0));
    row().key("logo").size(size, size).children(marks).a11y_hidden()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::Source;
    use rune::text::icon_coverage;

    const PX: u32 = 96;

    fn ink(mark: &Icon, x: f32, y: f32) -> u8 {
        let at = |v: f32| (v / mark.view * PX as f32) as u32;
        icon_coverage(mark, PX)[(at(y) * PX + at(x)) as usize]
    }

    #[test]
    fn every_logo_reaches_all_four_sides_of_its_box() {
        for source in Source::ALL {
            let drawn: Vec<(u32, u32)> = layers(source.into())
                .iter()
                .flat_map(|(mark, _)| icon_coverage(mark, PX).into_iter().enumerate().filter(|(_, c)| *c > 127).map(|(i, _)| (i as u32 % PX, i as u32 / PX)).collect::<Vec<_>>())
                .collect();
            let (xs, ys) = (drawn.iter().map(|p| p.0), drawn.iter().map(|p| p.1));
            let bounds = (xs.clone().min(), xs.max(), ys.clone().min(), ys.max());
            let (Some(left), Some(right), Some(top), Some(bottom)) = bounds else { panic!("{} draws nothing", source.label()) };
            assert!(left <= 2 && top <= 2 && right >= PX - 3 && bottom >= PX - 3, "{} sits at {bounds:?}", source.label());
        }
    }

    #[test]
    fn hyper_is_a_solid_pane_beside_a_hollow_one_spanning_its_box() {
        let drawn: Vec<(u32, u32)> = icon_coverage(&HYPER, PX).into_iter().enumerate().filter(|(_, c)| *c > 127).map(|(i, _)| (i as u32 % PX, i as u32 / PX)).collect();
        let (left, right) = (drawn.iter().map(|p| p.0).min().unwrap(), drawn.iter().map(|p| p.0).max().unwrap());
        let (top, bottom) = (drawn.iter().map(|p| p.1).min().unwrap(), drawn.iter().map(|p| p.1).max().unwrap());
        assert!(left <= 2 && right >= PX - 3, "{left}..{right}");
        assert!(top.abs_diff(PX - 1 - bottom) <= 1, "centred: {top}..{bottom}");
        assert_eq!(ink(&HYPER, 130.0, 280.0), 255, "the solid pane");
        assert_eq!(ink(&HYPER, 429.0, 280.0), 0, "inside the hollow one");
        assert_eq!(ink(&HYPER, 324.0, 280.0), 255, "its outline");
    }

    #[test]
    fn codex_is_a_solid_shape_with_the_prompt_cut_out() {
        assert_eq!(ink(&CODEX, 14.75, 8.5), 255, "the body");
        assert_eq!(ink(&CODEX, 15.2, 15.4), 0, "the underscore");
        assert_eq!(ink(&CODEX, 0.3, 0.3), 0, "outside it");
    }

    #[test]
    fn claude_is_a_burst_with_gaps_between_its_rays() {
        assert_eq!(ink(&CLAUDE, 6.0, 6.0), 255, "the centre");
        assert_eq!(ink(&CLAUDE, 0.5, 0.5), 0, "the corner");
    }

    /// The grid in Pi's `pi-logo.js`.
    #[test]
    fn pi_is_its_four_by_four_grid() {
        let cell = |x: usize, y: usize| {
            let on = |mark: &Icon| ink(mark, x as f32 + 0.5, y as f32 + 0.5) == 255;
            [(&PI_CORAL, 'c'), (&PI_BLUE, 'b'), (&PI_YELLOW, 'y')].into_iter().find(|(mark, _)| on(mark)).map_or('.', |(_, name)| name)
        };
        let grid: Vec<String> = (0..4).map(|y| (0..4).map(|x| cell(x, y)).collect()).collect();
        assert_eq!(grid, ["ccc.", "b.c.", "bb.y", "b..y"]);
    }
}
