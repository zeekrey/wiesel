# Third-party source attribution

`src/input.rs` is adapted from the GPUI text-input example maintained by Zed Industries:

- Source: https://github.com/zed-industries/zed/blob/f8c2cc844057540ca1eac7de4f19f50d7597dead/crates/gpui/examples/input.rs
- Copyright: Zed Industries and contributors
- License: Apache License, Version 2.0 (see `GPUI-APACHE-2.0.txt`)
- Modifications: reusable field API, dark styling, secret masking, horizontal caret scrolling, clipboard restrictions for secrets, Unicode/IME offset fixes, and tests.

`src/theme.rs` embeds Lucide SVG icon paths used by the Paper designs:

- Source: https://github.com/lucide-icons/lucide
- License: ISC / MIT (see `Lucide-LICENSE.txt`)

`resources/fonts/` embeds Inter and DM Mono from Google Fonts:

- Sources: https://github.com/google/fonts/tree/main/ofl/inter and https://github.com/google/fonts/tree/main/ofl/dmmono
- License: SIL Open Font License 1.1 (see `Inter-OFL.txt` and `DMMono-OFL.txt`)

Other Cargo dependencies retain their respective licenses. This notice does not relicense them or declare a license for the entire Wiesel application.
