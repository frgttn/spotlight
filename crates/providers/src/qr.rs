//! QR codes: Generate QR Code (a form, or `qr <text>` typed in root search)
//! and scanning the screen or the clipboard's image for codes. The launcher
//! draws the code and does the scanning (see `spotlight_qr`).

use spotlight_core::{
    item::{Command, Icon, QrSource, SearchItem},
    item_id,
    provider::Provider,
    view::{Field, FieldKind, FormView, View},
};
use spotlight_qr::GLYPH;

pub use spotlight_qr::ID;

/// Typed before the text in root search: `qr https://example.com`.
const PREFIX: &str = "qr ";

pub struct Qr;

impl Provider for Qr {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        "QR Codes"
    }

    fn description(&self) -> &'static str {
        "Generate QR Code (also “qr <text>”), and reading QR codes on the screen or the clipboard."
    }

    fn catalog(&self) -> Vec<SearchItem> {
        let form = FormView {
            title: "Generate QR Code".into(),
            fields: vec![Field {
                id: "text".into(),
                label: "Text".into(),
                kind: FieldKind::TextArea {
                    placeholder: "A link, Wi-Fi details, any text".into(),
                    default: String::new(),
                    required: true,
                },
            }],
            submit_title: "Generate".into(),
            submit: Command::ShowQrCode("{text}".into()),
        };
        vec![
            SearchItem::new(item_id(ID, "generate"), "Generate QR Code")
                .subtitle("QR Codes")
                .keywords(&["qr code", "create", "make", "barcode"])
                .icon(Icon::Glyph(GLYPH))
                .action("Open Form", Command::PushView(Box::new(View::Form(form)))),
            SearchItem::new(item_id(ID, "scan-screen"), "Scan QR Code from Screen")
                .subtitle("QR Codes")
                .keywords(&["qr code", "read", "decode", "barcode"])
                .icon(Icon::Glyph(GLYPH))
                .action("Scan Screen", Command::ScanQrCodes(QrSource::Screen)),
            SearchItem::new(item_id(ID, "scan-clipboard"), "Scan QR Code from Clipboard")
                .subtitle("QR Codes")
                .keywords(&["qr code", "read", "decode", "image", "barcode"])
                .icon(Icon::Glyph(GLYPH))
                .action("Scan Clipboard", Command::ScanQrCodes(QrSource::Clipboard)),
        ]
    }

    fn answers(&self, query: &str) -> Vec<SearchItem> {
        let text = query
            .get(..PREFIX.len())
            .filter(|prefix| prefix.eq_ignore_ascii_case(PREFIX))
            .map(|_| query[PREFIX.len()..].trim())
            .unwrap_or_default();
        if text.is_empty() {
            return Vec::new();
        }
        vec![
            SearchItem::new(item_id(ID, "answer"), "Generate QR Code")
                .subtitle(text)
                .icon(Icon::Glyph(GLYPH))
                .action("Show QR Code", Command::ShowQrCode(text.to_owned()))
                .action("Copy Image", Command::CopyQrCode(text.to_owned()))
                .action("Save Image…", Command::SaveQrCode(text.to_owned())),
        ]
    }
}

#[cfg(test)]
mod tests {
    use spotlight_core::view::FieldValue;

    use super::*;

    #[test]
    fn answers_after_the_prefix() {
        assert!(Qr.answers("qr").is_empty());
        assert!(Qr.answers("qr   ").is_empty());
        assert!(Qr.answers("qrcode").is_empty());
        let answers = Qr.answers("QR  https://example.com ");
        assert_eq!(
            answers[0].actions[0].command,
            Command::ShowQrCode("https://example.com".into())
        );
    }

    #[test]
    fn the_form_shows_what_was_typed() {
        let catalog = Qr.catalog();
        let Command::PushView(view) = &catalog[0].actions[0].command else {
            panic!("no form");
        };
        let View::Form(form) = &**view else {
            panic!("no form");
        };
        assert_eq!(
            form.fill(&[FieldValue::Text("line 1\nline 2".into())]),
            Command::ShowQrCode("line 1\nline 2".into())
        );
    }
}
