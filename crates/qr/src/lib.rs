//! QR codes: made from text with `qrcode`, read from images with `rqrr`,
//! both pure Rust. Also the views that show them, which the QR provider and
//! the launcher (for codes it scanned) both open.

use image::{GrayImage, ImageFormat, Luma, RgbaImage};
use qrcode::{Color, EcLevel, QrCode};
use spotlight_core::{
    item::{Action, CardKind, Command, Icon, SearchItem, WifiSecurity},
    item_id,
    quicklink::encode_value,
    view::{DetailView, ListView, View},
};

mod content;

pub use content::{Contact, Content, Email, Event, link, parse};

/// Light modules around the code, as the standard asks, so readers find it.
pub const QUIET_ZONE: usize = 4;
/// The most a code holds: version 40 at error correction level L, in bytes.
pub const MAX_BYTES: usize = 2953;
/// Pixels per module in saved and copied images.
const PNG_MODULE: u32 = 12;
/// Images larger than this are first read scaled down by a whole factor (a
/// screen of 3840 px becomes 1920): reading is linear in pixels, and large
/// codes shrink fine. Small ones may not, so when that finds nothing the
/// image is read again at full size, in tiles of this size.
const READ_LIMIT: u32 = 2600;
/// How much tiles overlap, so a code on the edge of one is whole in the next.
const TILE_OVERLAP: u32 = 600;

/// Which QR code views and rows are from: `"qr:{n}"`.
pub const ID: &str = "qr";

/// A QR code of some text: its modules, row by row, without the quiet zone.
pub struct Code {
    width: usize,
    dark: Vec<bool>,
    version: i16,
    level: EcLevel,
}

impl Code {
    /// The code for `text`, with as much error correction as fits: medium
    /// for most text, low for the longest.
    pub fn new(text: &str) -> Result<Self, String> {
        if text.is_empty() {
            return Err("Nothing to encode".into());
        }
        let code = [EcLevel::M, EcLevel::L]
            .into_iter()
            .find_map(|level| QrCode::with_error_correction_level(text, level).ok())
            .ok_or_else(|| {
                format!(
                    "Too long for a QR code: {} bytes, at most {MAX_BYTES}",
                    text.len()
                )
            })?;
        let version = match code.version() {
            qrcode::Version::Normal(n) | qrcode::Version::Micro(n) => n,
        };
        Ok(Self {
            width: code.width(),
            level: code.error_correction_level(),
            version,
            dark: code
                .into_colors()
                .into_iter()
                .map(|color| color == Color::Dark)
                .collect(),
        })
    }

    /// Modules per side, without the quiet zone.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Modules per side with the quiet zone on both sides.
    pub fn full_width(&self) -> usize {
        self.width + 2 * QUIET_ZONE
    }

    pub fn is_dark(&self, x: usize, y: usize) -> bool {
        self.dark[y * self.width + x]
    }

    /// "Version 3 (29 × 29)".
    pub fn describe_version(&self) -> String {
        format!("Version {} ({} × {})", self.version, self.width, self.width)
    }

    /// "Medium (15%)".
    pub fn describe_level(&self) -> &'static str {
        match self.level {
            EcLevel::L => "Low (7%)",
            EcLevel::M => "Medium (15%)",
            EcLevel::Q => "Quartile (25%)",
            EcLevel::H => "High (30%)",
        }
    }

    /// Black on white, `module` pixels per module, with the quiet zone.
    pub fn image(&self, module: u32) -> RgbaImage {
        let module = module.max(1);
        let side = self.full_width() as u32 * module;
        RgbaImage::from_fn(side, side, |x, y| {
            let (mx, my) = ((x / module) as usize, (y / module) as usize);
            let dark = (QUIET_ZONE..QUIET_ZONE + self.width).contains(&mx)
                && (QUIET_ZONE..QUIET_ZONE + self.width).contains(&my)
                && self.is_dark(mx - QUIET_ZONE, my - QUIET_ZONE);
            if dark {
                image::Rgba([0, 0, 0, 255])
            } else {
                image::Rgba([255, 255, 255, 255])
            }
        })
    }

    /// The image as a PNG file, for the clipboard or to save.
    pub fn png(&self) -> Result<Vec<u8>, String> {
        let mut png = Vec::new();
        self.image(PNG_MODULE)
            .write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
            .map_err(|err| format!("Couldn't make the image: {err}"))?;
        Ok(png)
    }

    pub fn png_size(&self) -> u32 {
        self.full_width() as u32 * PNG_MODULE
    }
}

/// What every QR code in `image` says, in the order found, each once. Codes
/// drawn light on dark (dark themes) are read too.
pub fn read(image: &RgbaImage) -> Vec<String> {
    let factor = image
        .width()
        .max(image.height())
        .div_ceil(READ_LIMIT)
        .max(1);
    let found = read_both_ways(&grayscale(image, factor));
    if !found.is_empty() || factor == 1 {
        return found;
    }
    let mut found = Vec::new();
    let step = READ_LIMIT - TILE_OVERLAP;
    for y in (0..image.height().saturating_sub(TILE_OVERLAP).max(1)).step_by(step as usize) {
        for x in (0..image.width().saturating_sub(TILE_OVERLAP).max(1)).step_by(step as usize) {
            let tile = image::imageops::crop_imm(
                image,
                x,
                y,
                READ_LIMIT.min(image.width() - x),
                READ_LIMIT.min(image.height() - y),
            )
            .to_image();
            for text in read_both_ways(&grayscale(&tile, 1)) {
                if !found.contains(&text) {
                    found.push(text);
                }
            }
        }
    }
    found
}

/// The codes in `gray`, or if none, in it inverted.
fn read_both_ways(gray: &GrayImage) -> Vec<String> {
    let found = read_gray(gray);
    if !found.is_empty() {
        return found;
    }
    let mut inverted = gray.clone();
    image::imageops::invert(&mut inverted);
    read_gray(&inverted)
}

/// Luma, scaled down by `factor`: each pixel the average of a square of them.
fn grayscale(image: &RgbaImage, factor: u32) -> GrayImage {
    let (width, height) = (image.width() / factor, image.height() / factor);
    GrayImage::from_fn(width, height, |x, y| {
        let mut sum = 0u32;
        for dy in 0..factor {
            for dx in 0..factor {
                let [r, g, b, a] = image.get_pixel(x * factor + dx, y * factor + dy).0;
                // Transparent pixels read as white, as they'd show on a page.
                let luma = (299 * u32::from(r) + 587 * u32::from(g) + 114 * u32::from(b)) / 1000;
                sum += (luma * u32::from(a) + 255 * (255 - u32::from(a))) / 255;
            }
        }
        Luma([(sum / (factor * factor)) as u8])
    })
}

fn read_gray(gray: &GrayImage) -> Vec<String> {
    let mut prepared = rqrr::PreparedImage::prepare_from_greyscale(
        gray.width() as usize,
        gray.height() as usize,
        |x, y| gray.get_pixel(x as u32, y as u32).0[0],
    );
    let mut found: Vec<String> = Vec::new();
    for grid in prepared.detect_grids() {
        if let Ok((_, text)) = grid.decode()
            && !text.is_empty()
            && !found.contains(&text)
        {
            found.push(text);
        }
    }
    found
}

/// The view of a QR code of `text`: what it says, and actions for what it
/// is (Connect to Network, Add to Contacts, Open Link…). `scanned`: it was
/// read from the screen or the clipboard, so using it comes first rather
/// than copying its image.
pub fn view(text: &str, scanned: bool) -> View {
    let content = parse(text);
    let mut metadata = details(&content);
    metadata.push(("Content".into(), content.kind().into()));
    metadata.push(("Characters".into(), text.chars().count().to_string()));
    if let Ok(code) = Code::new(text) {
        metadata.push(("Size".into(), code.describe_version()));
        metadata.push(("Error Correction".into(), code.describe_level().into()));
    }

    let copy_text = Action::new("Copy Text", Command::Copy(text.to_owned()));
    let copy_image = Action::new("Copy Image", Command::CopyQrCode(text.to_owned()));
    let save = Action::new("Save Image…", Command::SaveQrCode(text.to_owned()));
    let uses = uses(&content);
    let actions = match (scanned, &content) {
        // Plain text and links are mostly copied.
        (true, Content::Text | Content::Link(_)) => [vec![copy_text], uses, vec![copy_image, save]],
        (true, _) => [uses, vec![copy_text], vec![copy_image, save]],
        (false, _) => [vec![copy_image, copy_text, save], uses, Vec::new()],
    };
    View::Detail(DetailView {
        title: "QR Code".into(),
        qr: Some(text.to_owned()),
        markdown: code_block(text),
        metadata,
        actions: actions.concat(),
    })
}

/// What the code says, label by label: the network and its password, the
/// contact's name and number…
fn details(content: &Content) -> Vec<(String, String)> {
    let mut details: Vec<(&str, String)> = Vec::new();
    match content {
        Content::Wifi(network) => {
            details.push(("Network", network.ssid.clone()));
            let security = match network.security {
                WifiSecurity::Open => "None",
                WifiSecurity::Wep => "WEP",
                WifiSecurity::Wpa => "WPA/WPA2",
                WifiSecurity::Wpa3 => "WPA3",
            };
            details.push(("Security", security.into()));
            details.push(("Password", network.password.clone()));
            if network.hidden {
                details.push(("Hidden", "Yes".into()));
            }
        }
        Content::Contact(contact) => {
            details.push(("Name", contact.name.clone()));
            details.push(("Organization", contact.organization.clone()));
            details.push(("Phone", contact.phones.join(", ")));
            details.push(("Email", contact.emails.join(", ")));
            details.push(("Website", contact.website.clone()));
            details.push(("Address", contact.address.clone()));
        }
        Content::Email(email) => {
            details.push(("To", email.to.clone()));
            details.push(("Subject", email.subject.clone()));
        }
        Content::Phone(number) => details.push(("Number", number.clone())),
        Content::Sms { number, message } => {
            details.push(("Number", number.clone()));
            details.push(("Message", message.clone()));
        }
        Content::Place {
            latitude,
            longitude,
        } => {
            details.push(("Latitude", latitude.clone()));
            details.push(("Longitude", longitude.clone()));
        }
        Content::Event(event) => {
            details.push(("Event", event.title.clone()));
            details.push(("Starts", event.start.clone()));
            details.push(("Ends", event.end.clone()));
            details.push(("Location", event.location.clone()));
        }
        Content::Link(_) | Content::Text => {}
    }
    details
        .into_iter()
        .filter(|(_, value)| !value.trim().is_empty())
        .map(|(label, value)| (label.to_owned(), value))
        .collect()
}

/// Actions for what the code is.
fn uses(content: &Content) -> Vec<Action> {
    let copy = |title: &str, text: &str| {
        (!text.is_empty()).then(|| Action::new(title, Command::Copy(text.to_owned())))
    };
    let open = |title: &str, url: String| Some(Action::new(title, Command::OpenUrl(url)));
    let actions = match content {
        Content::Link(url) => vec![open("Open Link", url.clone())],
        Content::Wifi(network) => vec![
            Some(Action::new(
                "Connect to Network",
                Command::ConnectWifi(network.clone()),
            )),
            copy("Copy Password", &network.password),
            copy("Copy Network Name", &network.ssid),
        ],
        Content::Contact(contact) => {
            let phone = contact.phones.first().map(String::as_str).unwrap_or("");
            let email = contact.emails.first().map(String::as_str).unwrap_or("");
            vec![
                Some(Action::new(
                    "Add to Contacts",
                    Command::OpenCard {
                        kind: CardKind::Contact,
                        text: contact.vcard.clone(),
                    },
                )),
                copy("Copy Phone Number", phone),
                copy("Copy Email Address", email),
                (!email.is_empty())
                    .then(|| open("Send Email", format!("mailto:{email}")))
                    .flatten(),
                link(&contact.website).and_then(|url| open("Open Website", url.to_owned())),
            ]
        }
        Content::Email(email) => vec![
            open("Send Email", mailto(email)),
            copy("Copy Email Address", &email.to),
        ],
        Content::Phone(number) => vec![
            open("Call", format!("tel:{}", number.replace(' ', ""))),
            copy("Copy Phone Number", number),
        ],
        Content::Sms { number, message } => vec![
            copy("Copy Phone Number", number),
            copy("Copy Message", message),
        ],
        Content::Place {
            latitude,
            longitude,
        } => {
            let coordinates = format!("{latitude},{longitude}");
            vec![
                open(
                    "Open in Maps",
                    format!(
                        "https://www.google.com/maps/search/?api=1&query={}",
                        encode_value(&coordinates, true)
                    ),
                ),
                copy("Copy Coordinates", &coordinates),
            ]
        }
        Content::Event(event) => vec![
            Some(Action::new(
                "Add to Calendar",
                Command::OpenCard {
                    kind: CardKind::Event,
                    text: event.ics.clone(),
                },
            )),
            copy("Copy Event Title", &event.title),
        ],
        Content::Text => Vec::new(),
    };
    actions.into_iter().flatten().collect()
}

/// A `mailto:` URL that opens a new email as the code describes it.
fn mailto(email: &Email) -> String {
    let mut url = format!("mailto:{}", email.to.trim());
    let fields = [("subject", &email.subject), ("body", &email.body)];
    let query: Vec<String> = fields
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(name, value)| format!("{name}={}", encode_value(value, true)))
        .collect();
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    url
}

/// What a code scan found: one code opens as it is; several are listed,
/// each opening its own view.
pub fn scanned_view(codes: &[String]) -> Option<View> {
    match codes {
        [] => None,
        [text] => Some(view(text, true)),
        codes => Some(View::List(ListView {
            title: format!("{} QR Codes", codes.len()),
            placeholder: Some("Search QR codes…".into()),
            items: codes
                .iter()
                .enumerate()
                .map(|(ix, text)| {
                    let content = parse(text);
                    let mut item =
                        SearchItem::new(item_id(ID, &format!("scan{ix}")), first_line(text))
                            .subtitle(content.kind())
                            .icon(Icon::Glyph(GLYPH))
                            .action(
                                "Show QR Code",
                                Command::PushView(Box::new(view(text, true))),
                            )
                            .action("Copy Text", Command::Copy(text.clone()));
                    item.actions.extend(uses(&content));
                    item.keywords = vec![text.clone()];
                    item
                })
                .collect(),
            empty: None,
            filters: Vec::new(),
            live: false,
        })),
    }
}

/// The QR code symbol of Segoe Fluent Icons / Segoe MDL2 Assets.
pub const GLYPH: char = '\u{ED14}';

/// The text's first non-empty line, cut at 80 characters.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    match line.char_indices().nth(80) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

/// `text` as a fenced code block, as it is: the fence is longer than any run
/// of backticks inside.
fn code_block(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}\n{text}\n{fence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_what_it_makes() {
        for text in [
            "https://example.com/путь?q=1",
            "WIFI:S:Home;T:WPA;P:secret;;",
            "a",
            &"long text ".repeat(150),
        ] {
            let code = Code::new(text).unwrap();
            assert_eq!(read(&code.image(4)), [text], "{}", code.describe_version());
        }
    }

    #[test]
    fn reads_light_on_dark_and_several_codes() {
        let one = Code::new("first").unwrap().image(5);
        let two = Code::new("second").unwrap().image(5);
        let mut both = RgbaImage::from_pixel(
            one.width() + two.width() + 40,
            one.height().max(two.height()) + 20,
            image::Rgba([255, 255, 255, 255]),
        );
        image::imageops::overlay(&mut both, &one, 10, 10);
        image::imageops::overlay(&mut both, &two, i64::from(one.width()) + 30, 10);
        let mut found = read(&both);
        found.sort();
        assert_eq!(found, ["first", "second"]);

        let mut dark = Code::new("dark mode").unwrap().image(4);
        image::imageops::invert(&mut dark);
        assert_eq!(read(&dark), ["dark mode"]);
    }

    #[test]
    fn reads_a_code_in_a_large_image() {
        let code = Code::new("on a 4K screen").unwrap().image(6);
        let mut screen = RgbaImage::from_pixel(3840, 2160, image::Rgba([40, 44, 52, 255]));
        image::imageops::overlay(&mut screen, &code, 2900, 1500);
        assert_eq!(read(&screen), ["on a 4K screen"]);
    }

    #[test]
    fn reads_a_small_code_on_two_wide_screens() {
        // 2.5 pixels a module once scaled down: found in the full-size tiles.
        let code = Code::new("https://example.com/spotlight-selftest")
            .unwrap()
            .image(5);
        let mut screens = RgbaImage::from_pixel(4480, 1440, image::Rgba([30, 30, 30, 255]));
        image::imageops::overlay(&mut screens, &code, 2507, 413);
        assert_eq!(read(&screens), ["https://example.com/spotlight-selftest"]);
    }

    #[test]
    fn refuses_what_does_not_fit() {
        assert!(Code::new("").is_err());
        assert!(Code::new(&"x".repeat(MAX_BYTES)).is_ok());
        let err = Code::new(&"x".repeat(MAX_BYTES + 1)).err().unwrap();
        assert!(err.contains("Too long"), "{err}");
    }

    #[test]
    fn views_put_the_likely_action_first() {
        let View::Detail(made) = view("https://rust-lang.org", false) else {
            panic!("not a detail");
        };
        assert_eq!(
            made.actions[0].command,
            Command::CopyQrCode("https://rust-lang.org".into())
        );
        assert_eq!(made.qr.as_deref(), Some("https://rust-lang.org"));
        let View::Detail(scanned) = view("https://rust-lang.org", true) else {
            panic!("not a detail");
        };
        let titles: Vec<&str> = scanned.actions.iter().map(|a| a.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Copy Text", "Open Link", "Copy Image", "Save Image…"]
        );

        assert!(scanned_view(&[]).is_none());
        let Some(View::List(list)) = scanned_view(&["a".into(), "WIFI:S:x;;".into()]) else {
            panic!("not a list");
        };
        assert_eq!(list.items[1].subtitle.as_deref(), Some("Wi-Fi Network"));
    }

    #[test]
    fn views_act_on_what_the_code_is() {
        let titles = |text: &str, scanned: bool| -> Vec<String> {
            let View::Detail(detail) = view(text, scanned) else {
                panic!("not a detail");
            };
            detail.actions.into_iter().map(|a| a.title).collect()
        };
        let wifi = "WIFI:S:Home;T:WPA;P:secret;;";
        assert_eq!(
            titles(wifi, true),
            [
                "Connect to Network",
                "Copy Password",
                "Copy Network Name",
                "Copy Text",
                "Copy Image",
                "Save Image…"
            ]
        );
        assert_eq!(
            titles(wifi, false)[..3],
            ["Copy Image", "Copy Text", "Save Image…"]
        );
        let View::Detail(detail) = view(wifi, true) else {
            panic!("not a detail");
        };
        assert!(
            detail
                .metadata
                .contains(&("Password".into(), "secret".into()))
        );
        assert_eq!(
            detail.actions[0].command,
            Command::ConnectWifi(spotlight_core::item::WifiNetwork {
                ssid: "Home".into(),
                security: WifiSecurity::Wpa,
                password: "secret".into(),
                hidden: false,
            })
        );

        let View::Detail(email) = view("MATMSG:TO:a@b.c;SUB:Hi there;BODY:x&y;;", true) else {
            panic!("not a detail");
        };
        assert_eq!(
            email.actions[0].command,
            Command::OpenUrl("mailto:a@b.c?subject=Hi%20there&body=x%26y".into())
        );
        assert_eq!(titles("tel:+1 555", true)[0], "Call");
        assert_eq!(titles("geo:1.5,2", true)[0], "Open in Maps");
        assert_eq!(
            titles("MECARD:N:A;TEL:1;;", true)[..2],
            ["Add to Contacts", "Copy Phone Number"]
        );
    }

    #[test]
    fn fences_text_with_backticks() {
        assert_eq!(code_block("a"), "```\na\n```");
        assert_eq!(code_block("x ```` y"), "`````\nx ```` y\n`````");
        assert_eq!(link(" http://a.b/c "), Some("http://a.b/c"));
        assert_eq!(link("https://a b"), None);
        assert_eq!(link("ftp://a"), None);
    }
}
