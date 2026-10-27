//! What a QR code's text is: a link, a Wi-Fi network, a contact, an email,
//! a phone number, a text message, a place or an event, in the formats
//! phones and code generators write (ZXing's "barcode contents"), else text.

use spotlight_core::item::{WifiNetwork, WifiSecurity};

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Link(String),
    Wifi(WifiNetwork),
    Contact(Contact),
    Email(Email),
    Phone(String),
    Sms {
        number: String,
        message: String,
    },
    /// Latitude and longitude, as written (`52.52,13.40`).
    Place {
        latitude: String,
        longitude: String,
    },
    Event(Event),
    Text,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Contact {
    pub name: String,
    pub organization: String,
    pub phones: Vec<String>,
    pub emails: Vec<String>,
    pub website: String,
    pub address: String,
    /// The contact as a vCard, to add it to an app.
    pub vcard: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Email {
    pub to: String,
    pub subject: String,
    pub body: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Event {
    pub title: String,
    /// Readable: "2026-10-26 18:00 UTC".
    pub start: String,
    pub end: String,
    pub location: String,
    /// The event as an iCalendar file, to add it to a calendar.
    pub ics: String,
}

impl Content {
    /// "Link", "Wi-Fi Network", "Contact"…
    pub fn kind(&self) -> &'static str {
        match self {
            Content::Link(_) => "Link",
            Content::Wifi(_) => "Wi-Fi Network",
            Content::Contact(_) => "Contact",
            Content::Email(_) => "Email",
            Content::Phone(_) => "Phone Number",
            Content::Sms { .. } => "Text Message",
            Content::Place { .. } => "Location",
            Content::Event(_) => "Event",
            Content::Text => "Text",
        }
    }
}

/// What `text` is.
pub fn parse(text: &str) -> Content {
    let trimmed = text.trim();
    if let Some(url) = link(trimmed) {
        return Content::Link(url.to_owned());
    }
    let after = |prefix: &str| {
        trimmed
            .get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .map(|_| &trimmed[prefix.len()..])
    };
    let parsed = if let Some(rest) = after("WIFI:") {
        wifi(rest).map(Content::Wifi)
    } else if let Some(rest) = after("MECARD:") {
        Some(Content::Contact(mecard(rest)))
    } else if after("BEGIN:VCARD").is_some() {
        Some(Content::Contact(vcard(trimmed)))
    } else if after("BEGIN:VEVENT").is_some() || after("BEGIN:VCALENDAR").is_some() {
        event(trimmed).map(Content::Event)
    } else if let Some(rest) = after("MATMSG:") {
        Some(Content::Email(matmsg(rest)))
    } else if let Some(rest) = after("mailto:") {
        Some(Content::Email(mailto(rest)))
    } else if let Some(rest) = after("tel:") {
        Some(Content::Phone(decode_percent(rest).trim().to_owned()))
    } else if let Some(rest) = after("SMSTO:").or_else(|| after("MMSTO:")) {
        let (number, message) = rest.split_once(':').unwrap_or((rest, ""));
        Some(Content::Sms {
            number: number.trim().to_owned(),
            message: message.to_owned(),
        })
    } else if let Some(rest) = after("sms:") {
        let (number, query) = rest.split_once('?').unwrap_or((rest, ""));
        Some(Content::Sms {
            number: decode_percent(number).trim().to_owned(),
            message: query_value(query, "body").unwrap_or_default(),
        })
    } else if let Some(rest) = after("geo:") {
        place(rest)
    } else {
        None
    };
    parsed.unwrap_or(Content::Text)
}

/// The URL a code says, when it says one and nothing else.
pub fn link(text: &str) -> Option<&str> {
    let text = text.trim();
    let lower = text.get(..8).unwrap_or(text).to_ascii_lowercase();
    let is_url = (lower.starts_with("https://") || lower.starts_with("http://"))
        && !text.contains(char::is_whitespace);
    is_url.then_some(text)
}

/// `WIFI:T:WPA;S:Home;P:secret;H:true;;`, fields in any order.
fn wifi(rest: &str) -> Option<WifiNetwork> {
    let mut network = WifiNetwork::default();
    let mut kind = String::new();
    for (key, value) in fields(rest) {
        match key.to_ascii_uppercase().as_str() {
            "S" => network.ssid = value,
            "P" => network.password = value,
            "T" => kind = value,
            "H" => network.hidden = value.eq_ignore_ascii_case("true"),
            _ => {}
        }
    }
    network.security = match kind.to_ascii_uppercase().as_str() {
        "WEP" => WifiSecurity::Wep,
        "SAE" | "WPA3" => WifiSecurity::Wpa3,
        "NOPASS" => WifiSecurity::Open,
        "" if network.password.is_empty() => WifiSecurity::Open,
        _ => WifiSecurity::Wpa,
    };
    (!network.ssid.is_empty()).then_some(network)
}

/// `MECARD:N:Doe,John;TEL:123;EMAIL:a@b.c;;`
fn mecard(rest: &str) -> Contact {
    let mut contact = Contact::default();
    for (key, value) in fields(rest) {
        match key.to_ascii_uppercase().as_str() {
            // "Last,First".
            "N" => {
                contact.name = match value.split_once(',') {
                    Some((last, first)) => format!("{} {}", first.trim(), last.trim()),
                    None => value,
                }
            }
            "ORG" => contact.organization = value,
            "TEL" => contact.phones.push(value),
            "EMAIL" => contact.emails.push(value),
            "URL" => contact.website = value,
            "ADR" => contact.address = value,
            _ => {}
        }
    }
    contact.vcard = to_vcard(&contact);
    contact
}

/// A vCard (2.1 to 4.0): `FN`, `N`, `ORG`, `TEL`, `EMAIL`, `URL`, `ADR`.
fn vcard(text: &str) -> Contact {
    let mut contact = Contact::default();
    let mut structured = String::new();
    for (key, value) in properties(text) {
        match key.as_str() {
            "FN" => contact.name = value,
            "N" => structured = value,
            "ORG" => {
                contact.organization = value.replace(';', ", ").trim_matches([',', ' ']).into()
            }
            "TEL" => contact.phones.push(value),
            "EMAIL" => contact.emails.push(value),
            "URL" => contact.website = value,
            "ADR" => {
                let parts: Vec<&str> = value
                    .split(';')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .collect();
                contact.address = parts.join(", ");
            }
            _ => {}
        }
    }
    if contact.name.is_empty() {
        // "Last;First;Middle;Prefix;Suffix".
        let mut parts = structured.split(';');
        let (last, first) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        contact.name = format!("{first} {last}").trim().to_owned();
    }
    contact.vcard = text.trim().to_owned();
    contact
}

/// `MATMSG:TO:a@b.c;SUB:Hi;BODY:Text;;`
fn matmsg(rest: &str) -> Email {
    let mut email = Email::default();
    for (key, value) in fields(rest) {
        match key.to_ascii_uppercase().as_str() {
            "TO" => email.to = value,
            "SUB" => email.subject = value,
            "BODY" => email.body = value,
            _ => {}
        }
    }
    email
}

/// `mailto:a@b.c?subject=Hi&body=Text`
fn mailto(rest: &str) -> Email {
    let (to, query) = rest.split_once('?').unwrap_or((rest, ""));
    Email {
        to: decode_percent(to),
        subject: query_value(query, "subject").unwrap_or_default(),
        body: query_value(query, "body").unwrap_or_default(),
    }
}

/// `geo:52.52,13.40` (an altitude or `?q=` after it is ignored).
fn place(rest: &str) -> Option<Content> {
    let coordinates = rest.split(['?', ';']).next()?;
    let mut parts = coordinates.split(',').map(str::trim);
    let (latitude, longitude) = (parts.next()?, parts.next()?);
    let valid = |value: &str, limit: f64| value.parse::<f64>().is_ok_and(|v| v.abs() <= limit);
    (valid(latitude, 90.) && valid(longitude, 180.)).then(|| Content::Place {
        latitude: latitude.to_owned(),
        longitude: longitude.to_owned(),
    })
}

/// An iCalendar event: the first `VEVENT`'s title, times and place.
fn event(text: &str) -> Option<Event> {
    let mut event = Event::default();
    let mut inside = !text.to_ascii_uppercase().contains("BEGIN:VEVENT");
    for (key, value) in properties(text) {
        match key.as_str() {
            "BEGIN" if value.eq_ignore_ascii_case("VEVENT") => inside = true,
            "END" if value.eq_ignore_ascii_case("VEVENT") => break,
            _ if !inside => {}
            "SUMMARY" => event.title = value,
            "DTSTART" => event.start = readable_time(&value),
            "DTEND" => event.end = readable_time(&value),
            "LOCATION" => event.location = value,
            _ => {}
        }
    }
    if event.title.is_empty() && event.start.is_empty() {
        return None;
    }
    let text = text.trim();
    event.ics = if text.to_ascii_uppercase().starts_with("BEGIN:VCALENDAR") {
        text.to_owned()
    } else {
        // Calendars want the event wrapped in a calendar.
        format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Spotlight//QR//EN\r\n{text}\r\nEND:VCALENDAR"
        )
    };
    Some(event)
}

/// `20261026T180000Z` → `2026-10-26 18:00 UTC`; `20261026` → `2026-10-26`.
fn readable_time(value: &str) -> String {
    let digits = |range: std::ops::Range<usize>| {
        value
            .get(range)
            .filter(|part| part.bytes().all(|b| b.is_ascii_digit()))
    };
    let Some(date) = digits(0..4)
        .zip(digits(4..6))
        .zip(digits(6..8))
        .map(|((y, m), d)| format!("{y}-{m}-{d}"))
    else {
        return value.to_owned();
    };
    match (value.get(8..9), digits(9..11), digits(11..13)) {
        (Some("T"), Some(hour), Some(minute)) => {
            let zone = if value.ends_with('Z') { " UTC" } else { "" };
            format!("{date} {hour}:{minute}{zone}")
        }
        _ => date,
    }
}

/// The `KEY:value` fields of a MeCard-style text (`WIFI:`, `MECARD:`,
/// `MATMSG:`), split at `;` not escaped with `\`, values unescaped.
fn fields(rest: &str) -> Vec<(String, String)> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = rest.chars();
    let mut push = |field: &mut String| {
        if let Some((key, value)) = split_key(field) {
            fields.push((key, value));
        }
        field.clear();
    };
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                // Kept escaped until the key is split off.
                field.push('\\');
                field.extend(chars.next());
            }
            ';' => push(&mut field),
            c => field.push(c),
        }
    }
    push(&mut field);
    fields
}

/// `K:va\:lue` → (`K`, `va:lue`).
fn split_key(field: &str) -> Option<(String, String)> {
    let (key, value) = field.split_once(':')?;
    let mut unescaped = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => unescaped.extend(chars.next()),
            c => unescaped.push(c),
        }
    }
    Some((key.trim().to_owned(), unescaped))
}

/// The properties of a vCard or iCalendar text: unfolded lines, names in
/// capitals without their parameters (`TEL;TYPE=cell` → `TEL`), values
/// unescaped (`\n`, `\,`, `\;`).
fn properties(text: &str) -> Vec<(String, String)> {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        match (line.strip_prefix([' ', '\t']), lines.last_mut()) {
            // A folded line continues the one before.
            (Some(rest), Some(last)) => last.push_str(rest),
            _ => lines.push(line.to_owned()),
        }
    }
    lines
        .iter()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            let name = name.split(';').next()?.trim().to_ascii_uppercase();
            let value = value
                .trim()
                .replace("\\n", "\n")
                .replace("\\N", "\n")
                .replace("\\,", ",")
                .replace("\\;", ";")
                .replace("\\\\", "\\");
            Some((name, value))
        })
        .collect()
}

/// Value of `key` in a URL query (`subject=Hi&body=…`), decoded.
fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        name.eq_ignore_ascii_case(key)
            .then(|| decode_percent(&value.replace('+', " ")))
    })
}

/// `%20` and the like decoded, as UTF-8; anything malformed stays as it is.
fn decode_percent(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut ix = 0;
    while ix < bytes.len() {
        let hex = bytes
            .get(ix + 1..ix + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[ix], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                ix += 3;
            }
            (byte, _) => {
                out.push(byte);
                ix += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A vCard 3.0 of `contact`, for contacts that came as a MeCard.
fn to_vcard(contact: &Contact) -> String {
    let escape = |value: &str| {
        value
            .replace('\\', "\\\\")
            .replace(',', "\\,")
            .replace(';', "\\;")
            .replace('\n', "\\n")
    };
    let mut lines = vec!["BEGIN:VCARD".to_owned(), "VERSION:3.0".to_owned()];
    lines.push(format!("FN:{}", escape(&contact.name)));
    // N is required in 3.0: the whole name as the family name will do.
    lines.push(format!("N:{};;;;", escape(&contact.name)));
    if !contact.organization.is_empty() {
        lines.push(format!("ORG:{}", escape(&contact.organization)));
    }
    lines.extend(
        contact
            .phones
            .iter()
            .map(|phone| format!("TEL:{}", escape(phone))),
    );
    lines.extend(
        contact
            .emails
            .iter()
            .map(|email| format!("EMAIL:{}", escape(email))),
    );
    if !contact.website.is_empty() {
        lines.push(format!("URL:{}", escape(&contact.website)));
    }
    if !contact.address.is_empty() {
        lines.push(format!("ADR:;;{};;;;", escape(&contact.address)));
    }
    lines.push("END:VCARD".to_owned());
    lines.join("\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_wifi_networks() {
        let Content::Wifi(network) = parse(r"WIFI:T:WPA;S:My\;Net;P:pa\:ss\\word;H:true;;") else {
            panic!("not Wi-Fi");
        };
        assert_eq!(
            network,
            WifiNetwork {
                ssid: "My;Net".into(),
                security: WifiSecurity::Wpa,
                password: r"pa:ss\word".into(),
                hidden: true,
            }
        );
        let Content::Wifi(open) = parse("wifi:S:Cafe;T:nopass;P:;;") else {
            panic!("not Wi-Fi");
        };
        assert_eq!(open.security, WifiSecurity::Open);
        let Content::Wifi(sae) = parse("WIFI:S:New;T:SAE;P:x;;") else {
            panic!("not Wi-Fi");
        };
        assert_eq!(sae.security, WifiSecurity::Wpa3);
        assert_eq!(parse("WIFI:T:WPA;P:x;;"), Content::Text);
    }

    #[test]
    fn reads_contacts() {
        let Content::Contact(card) =
            parse("MECARD:N:Doe,John;TEL:+1 555 0100;EMAIL:john@doe.com;URL:https://doe.com;;")
        else {
            panic!("not a contact");
        };
        assert_eq!(card.name, "John Doe");
        assert_eq!(card.phones, ["+1 555 0100"]);
        assert_eq!(card.emails, ["john@doe.com"]);
        assert!(card.vcard.contains("FN:John Doe\r\n"), "{}", card.vcard);
        assert!(card.vcard.contains("TEL:+1 555 0100"));

        let vcard = "BEGIN:VCARD\nVERSION:3.0\nN:Иванова;Анна;;;\nORG:ACME\\, Inc.\nTEL;TYPE=cell:+7 900 000 00 00\nEMAIL:anna@acme.ru\nADR:;;Main St 1;Town;;123;\nEND:VCARD";
        let Content::Contact(card) = parse(vcard) else {
            panic!("not a contact");
        };
        assert_eq!(card.name, "Анна Иванова");
        assert_eq!(card.organization, "ACME, Inc.");
        assert_eq!(card.phones, ["+7 900 000 00 00"]);
        assert_eq!(card.address, "Main St 1, Town, 123");
        assert_eq!(card.vcard, vcard);
    }

    #[test]
    fn reads_messages_numbers_and_places() {
        assert_eq!(
            parse("mailto:a%40b.c?subject=Hello%20there&body=Hi+you"),
            Content::Email(Email {
                to: "a@b.c".into(),
                subject: "Hello there".into(),
                body: "Hi you".into(),
            })
        );
        assert_eq!(
            parse("MATMSG:TO:a@b.c;SUB:Re\\: x;BODY:y;;"),
            Content::Email(Email {
                to: "a@b.c".into(),
                subject: "Re: x".into(),
                body: "y".into(),
            })
        );
        assert_eq!(parse("tel:+15550100"), Content::Phone("+15550100".into()));
        assert_eq!(
            parse("SMSTO:+15550100:See you: at 5"),
            Content::Sms {
                number: "+15550100".into(),
                message: "See you: at 5".into(),
            }
        );
        assert_eq!(
            parse("geo:52.5200,13.4050,34?q=Berlin"),
            Content::Place {
                latitude: "52.5200".into(),
                longitude: "13.4050".into(),
            }
        );
        assert_eq!(parse("geo:200,1"), Content::Text);
        assert_eq!(parse("https://a.b/c").kind(), "Link");
        assert_eq!(parse("just words").kind(), "Text");
    }

    #[test]
    fn reads_events() {
        let text = "BEGIN:VEVENT\nSUMMARY:Standup\nDTSTART:20261027T090000Z\nDTEND:20261027T091500Z\nLOCATION:Room 4\nEND:VEVENT";
        let Content::Event(event) = parse(text) else {
            panic!("not an event");
        };
        assert_eq!(event.title, "Standup");
        assert_eq!(event.start, "2026-10-27 09:00 UTC");
        assert_eq!(event.end, "2026-10-27 09:15 UTC");
        assert_eq!(event.location, "Room 4");
        assert!(event.ics.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(event.ics.ends_with("END:VCALENDAR"));
        assert_eq!(readable_time("20261027"), "2026-10-27");
    }
}
