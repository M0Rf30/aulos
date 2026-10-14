// SPDX-FileCopyrightText: 2026 Gianluca Boiano
// SPDX-License-Identifier: GPL-3.0-only

//! Audioscrobbler 2.0 request signing.

/// Compute the `api_sig` for `params`: every parameter except `format` and
/// `callback`, sorted by name, concatenated as `name` + `value`, followed by
/// the shared secret, MD5-hashed (UTF-8, lowercase hex).
pub fn api_sig(params: &[(String, String)], secret: &str) -> String {
    let mut sorted: Vec<&(String, String)> = params
        .iter()
        .filter(|(k, _)| k != "format" && k != "callback" && k != "api_sig")
        .collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut buf = String::new();
    for (k, v) in sorted {
        buf.push_str(k);
        buf.push_str(v);
    }
    buf.push_str(secret);
    format!("{:x}", md5::compute(buf.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(k: &str, v: &str) -> (String, String) {
        (k.to_string(), v.to_string())
    }

    #[test]
    fn signature_is_md5_of_sorted_concatenation() {
        // Unsorted input; expected = md5("api_keyxxxmethodauth.getTokensecret")
        let params = [p("method", "auth.getToken"), p("api_key", "xxx")];
        let expected = format!("{:x}", md5::compute(b"api_keyxxxmethodauth.getTokensecret"));
        assert_eq!(api_sig(&params, "secret"), expected);
    }

    #[test]
    fn empty_input_hashes_empty_string() {
        assert_eq!(
            api_sig(&[], ""),
            "d41d8cd98f00b204e9800998ecf8427e",
            "md5 of the empty string"
        );
    }

    #[test]
    fn signature_is_lowercase_hex_32() {
        let sig = api_sig(&[p("a", "b")], "s");
        assert_eq!(sig.len(), 32);
        assert!(
            sig.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }

    #[test]
    fn format_and_callback_are_not_signed() {
        let a = [p("api_key", "k"), p("method", "m")];
        let b = [
            p("api_key", "k"),
            p("format", "json"),
            p("method", "m"),
            p("callback", "cb"),
        ];
        assert_eq!(api_sig(&a, "s"), api_sig(&b, "s"));
    }

    #[test]
    fn existing_api_sig_param_is_ignored() {
        let a = [p("api_key", "k")];
        let b = [p("api_key", "k"), p("api_sig", "stale")];
        assert_eq!(api_sig(&a, "s"), api_sig(&b, "s"));
    }

    #[test]
    fn indexed_batch_params_sort_bytewise() {
        let params = [
            p("track[1]", "b"),
            p("artist[0]", "a"),
            p("track[0]", "c"),
            p("artist[1]", "d"),
        ];
        let expected = format!(
            "{:x}",
            md5::compute("artist[0]aartist[1]dtrack[0]ctrack[1]bS".as_bytes())
        );
        assert_eq!(api_sig(&params, "S"), expected);
    }

    #[test]
    fn utf8_values_are_hashed_as_utf8() {
        let params = [p("artist", "Björk")];
        let expected = format!("{:x}", md5::compute("artistBjörkS".as_bytes()));
        assert_eq!(api_sig(&params, "S"), expected);
    }
}
