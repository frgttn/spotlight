//! Window management: what can be done to a window, and where the layouts
//! put it. The geometry is plain arithmetic on rectangles, so the platform
//! code only reads and moves windows.

/// A rectangle in screen pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> i32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height
    }

    /// The same size, centered in `area`.
    fn centered_in(self, area: Rect) -> Rect {
        Rect {
            x: area.x + (area.width - self.width) / 2,
            y: area.y + (area.height - self.height) / 2,
            ..self
        }
    }

    /// Moved (not resized, unless it's larger) to lie inside `area`.
    fn clamped_to(self, area: Rect) -> Rect {
        let width = self.width.min(area.width);
        let height = self.height.min(area.height);
        Rect {
            x: self.x.clamp(area.x, area.right() - width),
            y: self.y.clamp(area.y, area.bottom() - height),
            width,
            height,
        }
    }
}

/// A part of the screen's work area, in fractions of its width and height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

const fn area(x: f32, y: f32, width: f32, height: f32) -> Area {
    Area {
        x,
        y,
        width,
        height,
    }
}

const THIRD: f32 = 1. / 3.;

/// `settings` key: pixels left between windows placed by layouts, and
/// between them and the screen's edges (`"0"` by default).
pub const GAP_SETTING: &str = "window_gap";
/// `settings` key: whether running a half again goes on to two thirds, then
/// one third (`"false"` turns it off; on by default).
pub const CYCLE_SETTING: &str = "window_cycle";
/// The largest gap Settings offers.
pub const MAX_GAP: i32 = 64;
/// The gaps Settings offers, in pixels.
pub const GAPS: &[i32] = &[0, 4, 8, 12, 16, 24];

/// What the halves go on to when run again on a window already there:
/// two thirds, then one third, then the half again.
fn cycle_of(part: Area) -> Option<[Area; 3]> {
    let (x, y, w, h) = (part.x, part.y, part.width, part.height);
    let two = 2. * THIRD;
    Some(match (x, y, w, h) {
        (0., 0., 0.5, 1.) => [part, area(0., 0., two, 1.), area(0., 0., THIRD, 1.)],
        (0.5, 0., 0.5, 1.) => [part, area(THIRD, 0., two, 1.), area(two, 0., THIRD, 1.)],
        (0., 0., 1., 0.5) => [part, area(0., 0., 1., two), area(0., 0., 1., THIRD)],
        (0., 0.5, 1., 0.5) => [part, area(0., THIRD, 1., two), area(0., two, 1., THIRD)],
        _ => return None,
    })
}

/// The part a layout should take this time: the next in its cycle (see
/// [`cycle_of`]) when the window is already at one of its steps, otherwise
/// the layout's own.
pub fn cycled(part: Area, window: Rect, work: Rect, gap: i32) -> Area {
    let Some(steps) = cycle_of(part) else {
        return part;
    };
    // Some apps round their size a little (to whole lines of text).
    let near = |a: i32, b: i32| (a - b).abs() <= 8;
    let at = |step: Area| {
        place(WindowCommand::Area(step), window, work, gap).is_some_and(|rect| {
            near(rect.x, window.x)
                && near(rect.y, window.y)
                && near(rect.right(), window.right())
                && near(rect.bottom(), window.bottom())
        })
    };
    match steps.iter().position(|&step| at(step)) {
        Some(ix) => steps[(ix + 1) % steps.len()],
        None => part,
    }
}

/// Something done to one window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WindowCommand {
    /// Fills this part of the work area.
    Area(Area),
    Maximize,
    /// 90% of the work area, centered.
    AlmostMaximize,
    MaximizeWidth,
    MaximizeHeight,
    /// 60% of the work area, at most 1024 × 900, centered.
    ReasonableSize,
    Center,
    /// Back where it was before the first window command moved it.
    Restore,
    MakeLarger,
    MakeSmaller,
    MoveLeft,
    MoveRight,
    MoveTop,
    MoveBottom,
    /// To the next display (by position, left to right), in the same place
    /// relative to its work area.
    NextDisplay,
    PreviousDisplay,
    Minimize,
    /// Minimizes every app window on the display this one is on.
    MinimizeDisplay,
    ToggleAlwaysOnTop,
    /// The app's next (or previous) window, cycling through all of them.
    FocusNextAppWindow,
    FocusPreviousAppWindow,
    /// Brings it to the front (restoring it if minimized).
    Focus,
    /// Asks it to close, as its × button would.
    Close,
    /// Asks where to move it, in a form with its position now; the UI
    /// shows the form, which runs [`crate::item::Command::SetWindowFrame`].
    AskPosition,
    /// Asks what size to make it, the same way.
    AskSize,
}

impl WindowCommand {
    /// Whether it puts the window in a place worked out from where it is
    /// (a layout, a size, a nudge), as opposed to maximizing it, moving it
    /// to another display or doing something else to it.
    pub fn places(&self) -> bool {
        self.moves()
            && !matches!(
                self,
                WindowCommand::Maximize
                    | WindowCommand::NextDisplay
                    | WindowCommand::PreviousDisplay
            )
    }

    /// Whether it moves or resizes the window, so the place it had before
    /// is worth remembering for [`WindowCommand::Restore`].
    pub fn moves(&self) -> bool {
        !matches!(
            self,
            WindowCommand::Restore
                | WindowCommand::Minimize
                | WindowCommand::MinimizeDisplay
                | WindowCommand::ToggleAlwaysOnTop
                | WindowCommand::FocusNextAppWindow
                | WindowCommand::FocusPreviousAppWindow
                | WindowCommand::Focus
                | WindowCommand::Close
                | WindowCommand::AskPosition
                | WindowCommand::AskSize
        )
    }
}

/// The layouts, as Raycast names them: (instance id, title, keywords, command).
pub const LAYOUTS: &[(&str, &str, &[&str], WindowCommand)] = {
    use WindowCommand::Area as A;
    &[
        (
            "left-half",
            "Left Half",
            &["snap"],
            A(area(0., 0., 0.5, 1.)),
        ),
        (
            "right-half",
            "Right Half",
            &["snap"],
            A(area(0.5, 0., 0.5, 1.)),
        ),
        ("top-half", "Top Half", &[], A(area(0., 0., 1., 0.5))),
        ("bottom-half", "Bottom Half", &[], A(area(0., 0.5, 1., 0.5))),
        (
            "center-half",
            "Center Half",
            &[],
            A(area(0.25, 0., 0.5, 1.)),
        ),
        (
            "top-left-quarter",
            "Top Left Quarter",
            &["corner"],
            A(area(0., 0., 0.5, 0.5)),
        ),
        (
            "top-right-quarter",
            "Top Right Quarter",
            &["corner"],
            A(area(0.5, 0., 0.5, 0.5)),
        ),
        (
            "bottom-left-quarter",
            "Bottom Left Quarter",
            &["corner"],
            A(area(0., 0.5, 0.5, 0.5)),
        ),
        (
            "bottom-right-quarter",
            "Bottom Right Quarter",
            &["corner"],
            A(area(0.5, 0.5, 0.5, 0.5)),
        ),
        (
            "first-third",
            "First Third",
            &["left third"],
            A(area(0., 0., THIRD, 1.)),
        ),
        (
            "center-third",
            "Center Third",
            &[],
            A(area(THIRD, 0., THIRD, 1.)),
        ),
        (
            "last-third",
            "Last Third",
            &["right third"],
            A(area(2. * THIRD, 0., THIRD, 1.)),
        ),
        (
            "first-two-thirds",
            "First Two Thirds",
            &[],
            A(area(0., 0., 2. * THIRD, 1.)),
        ),
        (
            "center-two-thirds",
            "Center Two Thirds",
            &[],
            A(area(THIRD / 2., 0., 2. * THIRD, 1.)),
        ),
        (
            "last-two-thirds",
            "Last Two Thirds",
            &[],
            A(area(THIRD, 0., 2. * THIRD, 1.)),
        ),
        (
            "first-fourth",
            "First Fourth",
            &[],
            A(area(0., 0., 0.25, 1.)),
        ),
        (
            "second-fourth",
            "Second Fourth",
            &[],
            A(area(0.25, 0., 0.25, 1.)),
        ),
        (
            "third-fourth",
            "Third Fourth",
            &[],
            A(area(0.5, 0., 0.25, 1.)),
        ),
        (
            "last-fourth",
            "Last Fourth",
            &[],
            A(area(0.75, 0., 0.25, 1.)),
        ),
        (
            "first-three-fourths",
            "First Three Fourths",
            &[],
            A(area(0., 0., 0.75, 1.)),
        ),
        (
            "center-three-fourths",
            "Center Three Fourths",
            &[],
            A(area(0.125, 0., 0.75, 1.)),
        ),
        (
            "last-three-fourths",
            "Last Three Fourths",
            &[],
            A(area(0.25, 0., 0.75, 1.)),
        ),
        ("top-third", "Top Third", &[], A(area(0., 0., 1., THIRD))),
        (
            "middle-third",
            "Middle Third",
            &[],
            A(area(0., THIRD, 1., THIRD)),
        ),
        (
            "bottom-third",
            "Bottom Third",
            &[],
            A(area(0., 2. * THIRD, 1., THIRD)),
        ),
        (
            "top-two-thirds",
            "Top Two Thirds",
            &[],
            A(area(0., 0., 1., 2. * THIRD)),
        ),
        (
            "bottom-two-thirds",
            "Bottom Two Thirds",
            &[],
            A(area(0., THIRD, 1., 2. * THIRD)),
        ),
        (
            "top-left-sixth",
            "Top Left Sixth",
            &[],
            A(area(0., 0., THIRD, 0.5)),
        ),
        (
            "top-center-sixth",
            "Top Center Sixth",
            &[],
            A(area(THIRD, 0., THIRD, 0.5)),
        ),
        (
            "top-right-sixth",
            "Top Right Sixth",
            &[],
            A(area(2. * THIRD, 0., THIRD, 0.5)),
        ),
        (
            "bottom-left-sixth",
            "Bottom Left Sixth",
            &[],
            A(area(0., 0.5, THIRD, 0.5)),
        ),
        (
            "bottom-center-sixth",
            "Bottom Center Sixth",
            &[],
            A(area(THIRD, 0.5, THIRD, 0.5)),
        ),
        (
            "bottom-right-sixth",
            "Bottom Right Sixth",
            &[],
            A(area(2. * THIRD, 0.5, THIRD, 0.5)),
        ),
        (
            "maximize",
            "Maximize",
            &["fill", "full screen"],
            WindowCommand::Maximize,
        ),
        (
            "almost-maximize",
            "Almost Maximize",
            &[],
            WindowCommand::AlmostMaximize,
        ),
        (
            "maximize-width",
            "Maximize Width",
            &[],
            WindowCommand::MaximizeWidth,
        ),
        (
            "maximize-height",
            "Maximize Height",
            &[],
            WindowCommand::MaximizeHeight,
        ),
        (
            "reasonable-size",
            "Reasonable Size",
            &[],
            WindowCommand::ReasonableSize,
        ),
        ("center", "Center", &["middle"], WindowCommand::Center),
        (
            "restore",
            "Restore",
            &["undo", "previous size"],
            WindowCommand::Restore,
        ),
        (
            "make-larger",
            "Make Larger",
            &["grow", "bigger"],
            WindowCommand::MakeLarger,
        ),
        (
            "make-smaller",
            "Make Smaller",
            &["shrink"],
            WindowCommand::MakeSmaller,
        ),
        ("move-left", "Move Left", &[], WindowCommand::MoveLeft),
        ("move-right", "Move Right", &[], WindowCommand::MoveRight),
        ("move-top", "Move Top", &["move up"], WindowCommand::MoveTop),
        (
            "move-bottom",
            "Move Bottom",
            &["move down"],
            WindowCommand::MoveBottom,
        ),
        (
            "next-display",
            "Move to Next Display",
            &["monitor", "screen"],
            WindowCommand::NextDisplay,
        ),
        (
            "previous-display",
            "Move to Previous Display",
            &["monitor", "screen"],
            WindowCommand::PreviousDisplay,
        ),
        ("minimize", "Minimize", &["hide"], WindowCommand::Minimize),
        (
            "move-window",
            "Move Window…",
            &["position", "coordinates"],
            WindowCommand::AskPosition,
        ),
        (
            "resize-window",
            "Resize Window…",
            &["size", "width", "height"],
            WindowCommand::AskSize,
        ),
        (
            "minimize-display",
            "Minimize All on This Display",
            &["hide", "desktop", "monitor", "screen"],
            WindowCommand::MinimizeDisplay,
        ),
        (
            "toggle-always-on-top",
            "Toggle Always on Top",
            &["pin", "topmost"],
            WindowCommand::ToggleAlwaysOnTop,
        ),
        (
            "focus-next-app-window",
            "Focus Next App Window",
            &["cycle"],
            WindowCommand::FocusNextAppWindow,
        ),
        (
            "focus-previous-app-window",
            "Focus Previous App Window",
            &["cycle"],
            WindowCommand::FocusPreviousAppWindow,
        ),
    ]
};

/// How much of the work area Almost Maximize fills.
const ALMOST: f32 = 0.9;
/// Make Larger and Make Smaller change each side by this share of the work area.
const STEP: f32 = 0.05;
/// Make Smaller stops at this share of the work area.
const SMALLEST: f32 = 0.25;

/// Where `command` puts a window that is at `window` on a screen whose work
/// area is `work`. `None` for commands that don't place windows by
/// themselves (Restore, display moves, focus…).
pub fn place(command: WindowCommand, window: Rect, work: Rect, gap: i32) -> Option<Rect> {
    let scale = |share: f32, length: i32| (share * length as f32).round() as i32;
    let rect = match command {
        WindowCommand::Area(part) => {
            // Edges by rounding each one, so neighbours meet without a gap.
            let left = work.x + scale(part.x, work.width);
            let top = work.y + scale(part.y, work.height);
            let right = work.x + scale(part.x + part.width, work.width);
            let bottom = work.y + scale(part.y + part.height, work.height);
            // `gap` at the screen's edges, half of it either side of an
            // edge shared with a neighbour, so neighbours are `gap` apart.
            let inset = |edge: i32, outer: i32| if edge == outer { gap } else { gap / 2 };
            let left = left + inset(left, work.x);
            let top = top + inset(top, work.y);
            let right = right - inset(right, work.right());
            let bottom = bottom - inset(bottom, work.bottom());
            Rect::new(left, top, right - left, bottom - top)
        }
        WindowCommand::Maximize => work,
        WindowCommand::AlmostMaximize => Rect {
            width: scale(ALMOST, work.width),
            height: scale(ALMOST, work.height),
            ..work
        }
        .centered_in(work),
        WindowCommand::MaximizeWidth => Rect {
            x: work.x,
            width: work.width,
            ..window
        }
        .clamped_to(work),
        WindowCommand::MaximizeHeight => Rect {
            y: work.y,
            height: work.height,
            ..window
        }
        .clamped_to(work),
        WindowCommand::ReasonableSize => Rect {
            width: scale(0.6, work.width).min(1024),
            height: scale(0.6, work.height).min(900),
            ..work
        }
        .centered_in(work),
        WindowCommand::Center => window.clamped_to(work).centered_in(work),
        WindowCommand::MakeLarger | WindowCommand::MakeSmaller => {
            let sign = if command == WindowCommand::MakeLarger {
                1
            } else {
                -1
            };
            let dx = sign * scale(STEP, work.width);
            let dy = sign * scale(STEP, work.height);
            let width = (window.width + 2 * dx)
                .max(scale(SMALLEST, work.width))
                .min(work.width);
            let height = (window.height + 2 * dy)
                .max(scale(SMALLEST, work.height))
                .min(work.height);
            Rect {
                x: window.x - (width - window.width) / 2,
                y: window.y - (height - window.height) / 2,
                width,
                height,
            }
            .clamped_to(work)
        }
        WindowCommand::MoveLeft => Rect {
            x: work.x,
            ..window
        }
        .clamped_to(work),
        WindowCommand::MoveRight => Rect {
            x: work.right() - window.width,
            ..window
        }
        .clamped_to(work),
        WindowCommand::MoveTop => Rect {
            y: work.y,
            ..window
        }
        .clamped_to(work),
        WindowCommand::MoveBottom => Rect {
            y: work.bottom() - window.height,
            ..window
        }
        .clamped_to(work),
        _ => return None,
    };
    Some(rect)
}

/// Where a window goes when given numbers: `x` and `y` (from the top left
/// of `work`) and a `width` and `height`, each `None` to keep what it is.
/// `Err` says which one isn't a number of pixels.
pub fn frame_from(
    window: Rect,
    work: Rect,
    [x, y, width, height]: [&str; 4],
) -> Result<Rect, String> {
    let number = |text: &str, name: &str, positive: bool| -> Result<Option<i32>, String> {
        let text = text.trim().trim_end_matches("px").trim();
        if text.is_empty() {
            return Ok(None);
        }
        match text.parse::<i32>() {
            Ok(value) if !positive || value > 0 => Ok(Some(value)),
            _ => Err(format!("{name} must be a whole number of pixels")),
        }
    };
    Ok(Rect {
        x: number(x, "X", false)?.map_or(window.x, |x| work.x + x),
        y: number(y, "Y", false)?.map_or(window.y, |y| work.y + y),
        width: number(width, "Width", true)?.unwrap_or(window.width),
        height: number(height, "Height", true)?.unwrap_or(window.height),
    })
}

/// Where a window at `window` in work area `from` goes on a display whose
/// work area is `to`: the same place and size relative to the work area.
pub fn move_between(window: Rect, from: Rect, to: Rect) -> Rect {
    let rescale =
        |length: i32, from: i32, to: i32| (length as i64 * to as i64 / from.max(1) as i64) as i32;
    Rect {
        x: to.x + rescale(window.x - from.x, from.width, to.width),
        y: to.y + rescale(window.y - from.y, from.height, to.height),
        width: rescale(window.width, from.width, to.width),
        height: rescale(window.height, from.height, to.height),
    }
    .clamped_to(to)
}

/// The display after (`step` 1) or before (-1) `current` among `displays`
/// (work areas), ordered left to right, then top to bottom; wraps around.
pub fn neighbour_display(displays: &[Rect], current: Rect, step: i32) -> Option<Rect> {
    let mut sorted = displays.to_vec();
    sorted.sort_by_key(|display| (display.x, display.y));
    if sorted.len() < 2 {
        return None;
    }
    let ix = sorted.iter().position(|display| *display == current)?;
    let next = (ix as i32 + step).rem_euclid(sorted.len() as i32) as usize;
    Some(sorted[next])
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect {
        x: 0,
        y: 0,
        width: 1920,
        height: 1032,
    };

    fn layout(id: &str) -> WindowCommand {
        LAYOUTS
            .iter()
            .find(|(instance, ..)| *instance == id)
            .map(|&(.., command)| command)
            .unwrap()
    }

    #[test]
    fn places_areas() {
        let window = Rect::new(100, 100, 800, 600);
        let at = |id| place(layout(id), window, WORK, 0).unwrap();
        assert_eq!(at("left-half"), Rect::new(0, 0, 960, 1032));
        assert_eq!(at("right-half"), Rect::new(960, 0, 960, 1032));
        assert_eq!(at("bottom-right-quarter"), Rect::new(960, 516, 960, 516));
        assert_eq!(at("first-third"), Rect::new(0, 0, 640, 1032));
        assert_eq!(at("last-third"), Rect::new(1280, 0, 640, 1032));
        assert_eq!(at("center-two-thirds"), Rect::new(320, 0, 1280, 1032));
        // Thirds of an odd width still meet edge to edge.
        let odd = Rect::new(-1000, 0, 1001, 700);
        let first = place(layout("first-third"), window, odd, 0).unwrap();
        let center = place(layout("center-third"), window, odd, 0).unwrap();
        let last = place(layout("last-third"), window, odd, 0).unwrap();
        assert_eq!(first.right(), center.x);
        assert_eq!(center.right(), last.x);
        assert_eq!(last.right(), odd.right());
    }

    #[test]
    fn takes_numbers() {
        let window = Rect::new(100, 100, 800, 600);
        let work = Rect::new(1920, 40, 1920, 1000);
        assert_eq!(
            frame_from(window, work, ["10", " 20px ", "", ""]),
            Ok(Rect::new(1930, 60, 800, 600))
        );
        assert_eq!(
            frame_from(window, work, ["", "", "1024", "768"]),
            Ok(Rect::new(100, 100, 1024, 768))
        );
        assert!(frame_from(window, work, ["", "", "0", ""]).is_err());
        assert!(frame_from(window, work, ["left", "", "", ""]).is_err());
    }

    #[test]
    fn leaves_gaps() {
        let window = Rect::new(100, 100, 800, 600);
        let at = |id| place(layout(id), window, WORK, 10).unwrap();
        assert_eq!(at("left-half"), Rect::new(10, 10, 945, 1012));
        assert_eq!(at("right-half"), Rect::new(965, 10, 945, 1012));
        // Neighbours end up `gap` apart, the edges `gap` from the screen's.
        assert_eq!(at("right-half").x - at("left-half").right(), 10);
        assert_eq!(at("top-left-quarter"), Rect::new(10, 10, 945, 501));
        // Only layouts: the rest don't change.
        assert_eq!(place(layout("maximize"), window, WORK, 10), Some(WORK));
    }

    #[test]
    fn halves_cycle_through_thirds() {
        let Some(WindowCommand::Area(left)) = Some(layout("left-half")) else {
            panic!("not an area");
        };
        let mut window = Rect::new(100, 100, 800, 600);
        let mut widths = Vec::new();
        for _ in 0..4 {
            let part = cycled(left, window, WORK, 0);
            window = place(WindowCommand::Area(part), window, WORK, 0).unwrap();
            widths.push(window.width);
        }
        assert_eq!(widths, [960, 1280, 640, 960]);
        // A little off (an app rounding its size) still counts as there.
        let near = Rect {
            width: 955,
            ..place(WindowCommand::Area(left), window, WORK, 0).unwrap()
        };
        assert_ne!(cycled(left, near, WORK, 0), left);
        // Layouts without a cycle stay as they are.
        let Some(WindowCommand::Area(quarter)) = Some(layout("top-left-quarter")) else {
            panic!("not an area");
        };
        assert_eq!(cycled(quarter, window, WORK, 0), quarter);
    }

    #[test]
    fn places_the_rest() {
        let window = Rect::new(100, 100, 800, 600);
        let at = |id| place(layout(id), window, WORK, 0).unwrap();
        assert_eq!(at("maximize"), WORK);
        assert_eq!(at("almost-maximize"), Rect::new(96, 51, 1728, 929));
        assert_eq!(at("reasonable-size"), Rect::new(448, 206, 1024, 619));
        assert_eq!(at("center"), Rect::new(560, 216, 800, 600));
        assert_eq!(at("maximize-width"), Rect::new(0, 100, 1920, 600));
        assert_eq!(at("move-right"), Rect::new(1120, 100, 800, 600));
        assert_eq!(at("move-bottom"), Rect::new(100, 432, 800, 600));
        let larger = at("make-larger");
        assert_eq!((larger.width, larger.height), (992, 704));
        let smaller = place(layout("make-smaller"), Rect::new(0, 0, 500, 300), WORK, 0).unwrap();
        assert_eq!((smaller.width, smaller.height), (480, 258));
        assert_eq!(place(layout("restore"), window, WORK, 0), None);
        // A window larger than the screen is shrunk to fit when centered.
        let huge = Rect::new(-50, -50, 3000, 2000);
        assert_eq!(place(WindowCommand::Center, huge, WORK, 0), Some(WORK));
    }

    #[test]
    fn moves_between_displays() {
        let right = Rect::new(1920, 0, 2560, 1400);
        let window = Rect::new(960, 0, 960, 1032);
        assert_eq!(
            move_between(window, WORK, right),
            Rect::new(1920 + 1280, 0, 1280, 1400)
        );
        let left = Rect::new(-1280, 200, 1280, 984);
        let displays = [right, WORK, left];
        assert_eq!(neighbour_display(&displays, WORK, 1), Some(right));
        assert_eq!(neighbour_display(&displays, WORK, -1), Some(left));
        assert_eq!(neighbour_display(&displays, right, 1), Some(left));
        assert_eq!(neighbour_display(&[WORK], WORK, 1), None);
    }

    #[test]
    fn layout_ids_are_unique() {
        let mut ids: Vec<&str> = LAYOUTS.iter().map(|(id, ..)| *id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), LAYOUTS.len());
    }
}
