/// Parse `data:<mime>;base64,<data>` → `(mime, data)`.
pub(crate) fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    Some((mime, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_data_url_jpeg() {
        let (mime, data) = parse_data_url("data:image/jpeg;base64,abc123").unwrap();
        assert_eq!(mime, "image/jpeg");
        assert_eq!(data, "abc123");
    }

    #[test]
    fn parse_data_url_rejects_non_base64() {
        assert!(parse_data_url("https://example.com/img.png").is_none());
        assert!(parse_data_url("data:image/jpeg,abc").is_none());
    }
}
